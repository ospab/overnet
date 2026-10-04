package com.ospab.overnet

import android.content.Context
import org.json.JSONObject
import org.mozilla.geckoview.GeckoRuntime
import org.mozilla.geckoview.GeckoRuntimeSettings
import java.io.File

data class Status(val running: Boolean, val relays: Int, val token: String, val error: String)

/**
 * One gateway and one Gecko per process. Gecko gets its preferences from a
 * config file written before it starts: everything goes through the gateway
 * over SOCKS5, names included, and nothing falls back to a direct connection.
 */
object Engine {
    private var runtime: GeckoRuntime? = null

    fun runtime(context: Context): GeckoRuntime {
        runtime?.let { return it }
        // If the gateway did not start, port 1 refuses everything: no page
        // leaves the device past overnet.
        val port = Gateway.start().takeIf { it > 0 } ?: 1
        val config = File(context.filesDir, "geckoview-config.yaml")
        config.writeText(configYaml(port))
        val settings = GeckoRuntimeSettings.Builder()
            .configFilePath(config.absolutePath)
            .aboutConfigEnabled(false)
            .consoleOutput(false)
            .preferredColorScheme(GeckoRuntimeSettings.COLOR_SCHEME_DARK)
            .build()
        return GeckoRuntime.create(context.applicationContext, settings).also { runtime = it }
    }

    fun status(): Status {
        val j = JSONObject(Gateway.status())
        return Status(j.optBoolean("running"), j.optInt("relays"), j.optString("token"), j.optString("error"))
    }

    // The same as browser/files/overnet-prefs.js on the desktop, plus what
    // Mullvad Browser does there and GeckoView doesn't: no WebRTC (it can reveal
    // the real address past the proxy), no geolocation, fingerprinting resistance.
    private fun configYaml(port: Int) = """
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
