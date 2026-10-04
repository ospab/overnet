package com.ospab.overnet

import android.net.Uri
import java.net.URLEncoder

/**
 * Only .ov opens here. A regular site goes to `browser.ov/go`, which explains
 * why and offers to open it in the regular browser — a direct connection from
 * here would put the real address next to the .ov visits. A search on a
 * regular engine goes to search.ov instead.
 */
object Rules {
    const val HOME = "http://browser.ov/"

    /** What was typed into the address bar: an address or a search on search.ov. */
    fun fromTyped(typed: String): String? {
        val t = typed.trim()
        if (t.isEmpty()) return null
        val url = when {
            t.contains("://") -> t
            !t.contains(' ') && t.contains('.') -> "http://$t"
            else -> search(t)
        }
        return redirect(url) ?: url
    }

    fun search(q: String) = "http://search.ov/?q=" + enc(q)

    /** Where a page load goes instead, or null when it may go ahead. */
    fun redirect(url: String): String? {
        val uri = Uri.parse(url)
        if (uri.scheme != "http" && uri.scheme != "https") return null
        val host = uri.host?.lowercase() ?: return null
        if (host.endsWith(".ov") || host == "ov") return null
        searchQuery(host, uri)?.let { return search(it) }
        return "http://browser.ov/go?url=" + enc(url)
    }

    /**
     * "Open in my regular browser" on the stub page: the target, if the link
     * carries the gateway's token (a page can't make one up).
     */
    fun openTarget(uri: Uri, token: String): String? {
        if (uri.host != "browser.ov" || uri.path != "/open") return null
        val target = uri.getQueryParameter("url") ?: return ""
        if (token.isEmpty() || uri.getQueryParameter("t") != token) return ""
        if (!target.startsWith("http://") && !target.startsWith("https://")) return ""
        return target
    }

    /** The address as the bar shows it: no scheme, nothing for the start page. */
    fun display(url: String): String {
        if (url == HOME || url.isEmpty() || url == "about:blank") return ""
        // The stub page for a regular site: the site it stands for.
        if (url.startsWith("http://browser.ov/go?")) {
            Uri.parse(url).getQueryParameter("url")?.let { return display(it) }
        }
        var s = url.removePrefix("http://").removePrefix("https://")
        if (s.indexOf('/') == s.length - 1) s = s.dropLast(1)
        return s
    }

    // Search engines, by host part and query parameter.
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
}
