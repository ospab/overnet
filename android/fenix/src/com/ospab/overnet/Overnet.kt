package com.ospab.overnet

import android.content.ActivityNotFoundException
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import mozilla.components.concept.engine.request.RequestInterceptor.InterceptionResponse
import org.json.JSONObject
import java.io.File
import java.net.URLEncoder

/**
 * overnet inside Firefox for Android (Fenix): the gateway, Gecko's settings and
 * the "only .ov here" rule. android/fenix/patch.py wires it into Fenix:
 * GeckoProvider passes [configFile] to Gecko, AppRequestInterceptor asks
 * [intercept] about every page load first.
 */
object Overnet {
    /**
     * Starts the gateway and writes Gecko's preferences for it; the path goes to
     * GeckoRuntimeSettings.configFilePath. Everything goes through the gateway
     * over SOCKS5, names included, and nothing falls back to a direct connection.
     */
    fun configFile(context: Context): String {
        // If the gateway did not start, port 1 refuses everything: no page
        // leaves the device past overnet.
        val port = Gateway.start().takeIf { it > 0 } ?: 1
        val file = File(context.filesDir, "overnet-geckoview.yaml")
        file.writeText(prefs(port))
        return file.absolutePath
    }

    /**
     * A page load: null lets it through. A regular site becomes its stub page
     * `browser.ov/go` (a direct connection would put the real address next to
     * the .ov visits), a search on a regular engine goes to search.ov, and
     * "open in my regular browser" on the stub page opens the system's browser.
     */
    fun intercept(context: Context, url: String, isSubframe: Boolean): InterceptionResponse? {
        val uri = Uri.parse(url)
        if (uri.scheme != "http" && uri.scheme != "https") return null
        val host = uri.host?.lowercase() ?: return null
        if (host == "browser.ov" && uri.path == "/open") {
            openExternal(context, uri)
            return InterceptionResponse.Deny
        }
        // Frames from regular sites inside a .ov page: the gateway refuses them.
        if (host.endsWith(".ov") || isSubframe) return null
        searchQuery(host, uri)?.let {
            return InterceptionResponse.Url("http://search.ov/?q=" + enc(it))
        }
        return InterceptionResponse.Url("http://browser.ov/go?url=" + enc(url))
    }

    private fun openExternal(context: Context, uri: Uri) {
        val target = uri.getQueryParameter("url") ?: return
        val token = JSONObject(Gateway.status()).optString("token")
        if (token.isEmpty() || uri.getQueryParameter("t") != token) return
        if (!target.startsWith("http://") && !target.startsWith("https://")) return
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(target))
            .addCategory(Intent.CATEGORY_BROWSABLE)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        // Not back into this browser (its link activities are aliases, so all
        // of ours that take the link): it would land on the stub again.
        val own = context.packageManager.queryIntentActivities(intent, 0)
            .filter { it.activityInfo.packageName == context.packageName }
            .map { ComponentName(it.activityInfo.packageName, it.activityInfo.name) }
        val chooser = Intent.createChooser(intent, null)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            .putExtra(Intent.EXTRA_EXCLUDE_COMPONENTS, own.toTypedArray())
        try {
            context.startActivity(chooser)
        } catch (e: ActivityNotFoundException) {
            // No other browser: the stub page stays.
        }
    }

    // The search engines Firefox offers, by host part and query parameter.
    private val engines = listOf(
        "google." to "q", "bing.com" to "q", "duckduckgo.com" to "q", "ecosia.org" to "q",
        "qwant.com" to "q", "startpage.com" to "query", "search.brave.com" to "q",
        "yandex." to "text", "ya.ru" to "text", "search.yahoo.com" to "p", "baidu.com" to "wd",
    )

    private fun searchQuery(host: String, uri: Uri): String? {
        val (_, param) = engines.firstOrNull { (h, _) -> host.contains(h) } ?: return null
        return uri.getQueryParameter(param)?.takeIf { it.isNotBlank() }
    }

    private fun enc(s: String) = URLEncoder.encode(s, "UTF-8")

    // As browser/files/overnet-prefs.js on the desktop, plus what Mullvad
    // Browser does there: no WebRTC (it can reveal the real address past the
    // proxy), no geolocation, fingerprinting resistance.
    private fun prefs(port: Int) = """
        prefs:
          network.proxy.type: 1
          network.proxy.socks: "127.0.0.1"
          network.proxy.socks_port: $port
          network.proxy.socks_version: 5
          network.proxy.socks_remote_dns: true
          network.proxy.socks5_remote_dns: true
          network.proxy.no_proxies_on: ""
          network.proxy.allow_hijacking_localhost: true
          network.proxy.failover_direct: false
          network.trr.mode: 5
          network.http.speculative-parallel-limit: 0
          network.prefetch-next: false
          network.dns.disablePrefetch: true
          browser.fixup.domainsuffixwhitelist.ov: true
          dom.security.https_only_mode: false
          dom.security.https_first: false
          dom.securecontext.allowlist: "mail.ov"
          media.peerconnection.enabled: false
          geo.enabled: false
          privacy.resistFingerprinting: true
    """.trimIndent() + "\n"
}
