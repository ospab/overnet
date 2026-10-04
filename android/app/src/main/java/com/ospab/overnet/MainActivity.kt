package com.ospab.overnet

import android.app.Activity
import android.app.AlertDialog
import android.content.ActivityNotFoundException
import android.content.Intent
import android.graphics.Typeface
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.TypedValue
import android.view.Gravity
import android.view.KeyEvent
import android.view.View
import android.view.WindowInsets
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import android.window.OnBackInvokedDispatcher
import org.mozilla.geckoview.AllowOrDeny
import org.mozilla.geckoview.GeckoResult
import org.mozilla.geckoview.GeckoSession
import org.mozilla.geckoview.GeckoSession.NavigationDelegate
import org.mozilla.geckoview.GeckoSession.NavigationDelegate.LoadRequest
import org.mozilla.geckoview.GeckoSessionSettings
import org.mozilla.geckoview.GeckoView
import org.mozilla.geckoview.GeckoSession.PermissionDelegate
import java.net.URLEncoder

private const val HOME = "http://browser.ov/"

/**
 * overnet browser: one page, an address bar and the network indicator.
 *
 * Only .ov opens here. A regular site goes to `browser.ov/go`, which explains
 * why and offers to open it in the regular browser — a direct connection from
 * here would put the real address next to the .ov visits.
 */
class MainActivity : Activity() {
    private lateinit var session: GeckoSession
    private lateinit var address: EditText
    private lateinit var dot: TextView
    private var canGoBack = false
    private val handler = Handler(Looper.getMainLooper())
    private val poll = object : Runnable {
        override fun run() {
            showStatus()
            handler.postDelayed(this, 3000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val runtime = Engine.runtime(this)
        session = GeckoSession(GeckoSessionSettings.Builder().build())
        session.navigationDelegate = Navigation()
        session.open(runtime)

        val view = GeckoView(this)
        view.setSession(session)
        setContentView(layout(view))

        if (Build.VERSION.SDK_INT >= 33) {
            onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT) { back() }
        }
        session.loadUri(HOME)
    }

    override fun onResume() {
        super.onResume()
        handler.post(poll)
    }

    override fun onPause() {
        handler.removeCallbacks(poll)
        super.onPause()
    }

    override fun onDestroy() {
        session.close()
        super.onDestroy()
    }

    @Deprecated("Android 12 and older; newer versions use the callback from onCreate")
    override fun onBackPressed() = back()

    private fun back() {
        if (canGoBack) session.goBack() else moveTaskToBack(true)
    }

    private fun dp(v: Int) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v.toFloat(), resources.displayMetrics).toInt()

    private fun layout(page: View): View {
        dot = TextView(this).apply {
            text = "●"
            textSize = 18f
            gravity = Gravity.CENTER
            setTextColor(getColor(R.color.muted))
            setPadding(dp(14), 0, dp(10), 0)
            setOnClickListener { showMenu() }
        }
        address = EditText(this).apply {
            hint = getString(R.string.address_hint)
            setHintTextColor(getColor(R.color.muted))
            setTextColor(getColor(R.color.text))
            textSize = 15f
            typeface = Typeface.DEFAULT
            isSingleLine = true
            imeOptions = EditorInfo.IME_ACTION_GO
            inputType = EditorInfo.TYPE_CLASS_TEXT or EditorInfo.TYPE_TEXT_VARIATION_URI
            setBackgroundColor(getColor(R.color.bar_field))
            setPadding(dp(12), 0, dp(12), 0)
            setSelectAllOnFocus(true)
            setOnEditorActionListener { v, action, event ->
                val enter = event?.keyCode == KeyEvent.KEYCODE_ENTER && event.action == KeyEvent.ACTION_DOWN
                if (action == EditorInfo.IME_ACTION_GO || enter) {
                    go(v.text.toString())
                    true
                } else {
                    false
                }
            }
        }
        val bar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setBackgroundColor(getColor(R.color.bar_bg))
            setPadding(0, dp(6), dp(8), dp(6))
            addView(dot, LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.MATCH_PARENT))
            addView(address, LinearLayout.LayoutParams(0, dp(40), 1f))
        }
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(getColor(R.color.bar_bg))
            addView(bar, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT))
            addView(page, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
        }
        // Android 15+ draws apps under the system bars and the keyboard.
        if (Build.VERSION.SDK_INT >= 35) {
            root.setOnApplyWindowInsetsListener { v, insets ->
                val b = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.ime())
                v.setPadding(b.left, b.top, b.right, b.bottom)
                WindowInsets.CONSUMED
            }
        }
        return root
    }

    /** What was typed: an address, a regular site (to its stub page) or a search on search.ov. */
    private fun go(typed: String) {
        val t = typed.trim()
        if (t.isEmpty()) return
        val url = when {
            t.contains("://") -> t
            !t.contains(' ') && t.contains('.') -> "http://$t"
            else -> "http://search.ov/?q=" + URLEncoder.encode(t, "UTF-8")
        }
        session.loadUri(stubIfRegular(url) ?: url)
        address.clearFocus()
        getSystemService(InputMethodManager::class.java).hideSoftInputFromWindow(address.windowToken, 0)
    }

    /** `browser.ov/go?url=…` for a regular http(s) site, null for .ov. */
    private fun stubIfRegular(url: String): String? {
        val uri = Uri.parse(url)
        if (uri.scheme != "http" && uri.scheme != "https") return null
        val host = uri.host?.lowercase() ?: return null
        if (host.endsWith(".ov")) return null
        return "http://browser.ov/go?url=" + URLEncoder.encode(url, "UTF-8")
    }

    private fun openExternal(url: String) {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addCategory(Intent.CATEGORY_BROWSABLE))
        } catch (e: ActivityNotFoundException) {
            Toast.makeText(this, R.string.no_app, Toast.LENGTH_SHORT).show()
        }
    }

    private fun showStatus() {
        val s = Engine.status()
        val color = when {
            !s.running -> R.color.bad
            s.relays > 0 -> R.color.ok
            else -> R.color.muted
        }
        dot.setTextColor(getColor(color))
    }

    private fun showMenu() {
        val s = Engine.status()
        val state = when {
            !s.running -> getString(R.string.status_failed, s.error)
            s.relays > 0 -> getString(R.string.status_connected, s.relays)
            else -> getString(R.string.status_connecting)
        }
        AlertDialog.Builder(this)
            .setTitle(R.string.app_name)
            .setMessage(state + "\n\n" + getString(R.string.status_regular_sites))
            .setPositiveButton(R.string.new_circuits) { _, _ ->
                Gateway.newCircuits()
                Toast.makeText(this, R.string.new_circuits_done, Toast.LENGTH_SHORT).show()
            }
            .setNeutralButton(R.string.home) { _, _ -> session.loadUri(HOME) }
            .setNegativeButton(R.string.close, null)
            .show()
    }

    private inner class Navigation : NavigationDelegate {
        override fun onLocationChange(
            session: GeckoSession,
            url: String?,
            perms: MutableList<PermissionDelegate.ContentPermission>,
            hasUserGesture: Boolean,
        ) {
            if (!address.hasFocus()) address.setText(url ?: "")
        }

        override fun onCanGoBack(session: GeckoSession, canGoBack: Boolean) {
            this@MainActivity.canGoBack = canGoBack
        }

        override fun onLoadRequest(session: GeckoSession, request: LoadRequest): GeckoResult<AllowOrDeny> {
            val uri = Uri.parse(request.uri)
            when (uri.scheme) {
                "http", "https" -> {}
                "about", "data", "blob", "javascript" -> return GeckoResult.allow()
                else -> return GeckoResult.deny() // tel:, intent:, file: and the rest
            }
            // "Open in my regular browser" on the stub page: the gateway can't
            // start an app on Android, so the app does it, with the same token check.
            if (uri.host == "browser.ov" && uri.path == "/open") {
                val target = uri.getQueryParameter("url")
                val token = Engine.status().token
                if (target != null && token.isNotEmpty() && uri.getQueryParameter("t") == token &&
                    (target.startsWith("http://") || target.startsWith("https://"))
                ) {
                    handler.post { openExternal(target) }
                }
                return GeckoResult.deny()
            }
            val next = stubIfRegular(request.uri)
                ?: if (request.target == NavigationDelegate.TARGET_WINDOW_NEW) request.uri else null
            if (next != null) {
                // No tabs: a new window opens here, a regular site becomes its stub.
                handler.post { session.loadUri(next) }
                return GeckoResult.deny()
            }
            return GeckoResult.allow()
        }
    }
}
