package com.ospab.overnet

import android.app.Activity
import android.app.AlertDialog
import android.app.Dialog
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.KeyEvent
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.window.OnBackInvokedDispatcher
import org.mozilla.geckoview.AllowOrDeny
import org.mozilla.geckoview.GeckoResult
import org.mozilla.geckoview.GeckoRuntime
import org.mozilla.geckoview.GeckoSession
import org.mozilla.geckoview.GeckoSession.ContentDelegate
import org.mozilla.geckoview.GeckoSession.NavigationDelegate
import org.mozilla.geckoview.GeckoSession.NavigationDelegate.LoadRequest
import org.mozilla.geckoview.GeckoSession.PermissionDelegate
import org.mozilla.geckoview.GeckoSession.ProgressDelegate
import org.mozilla.geckoview.GeckoSessionSettings
import org.mozilla.geckoview.GeckoView
import org.mozilla.geckoview.WebResponse

/**
 * overnet browser: tabs, the address bar at the bottom, a menu, and the
 * network indicator — the whole interface is ours, Gecko only draws pages.
 * See [Rules] for what opens here and what doesn't.
 */
class MainActivity : Activity() {
    companion object {
        const val PICK_FILES = 1
    }

    private lateinit var runtime: GeckoRuntime
    private lateinit var gecko: GeckoView
    private lateinit var prompts: Prompts
    private val tabs = ArrayList<Tab>()
    private var current: Tab? = null

    private lateinit var top: FrameLayout
    private lateinit var bar: View
    private lateinit var progress: ProgressLine
    private lateinit var address: EditText
    private lateinit var dot: Glyph
    private lateinit var action: ImageView
    private lateinit var actionGlyph: Glyph
    private lateinit var tabCount: TextView
    private lateinit var panel: FrameLayout
    private lateinit var snack: LinearLayout

    private val handler = Handler(Looper.getMainLooper())
    private val poll = object : Runnable {
        override fun run() {
            showStatus()
            handler.postDelayed(this, 3000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        runtime = Engine.runtime(this)
        prompts = Prompts(this)
        gecko = GeckoView(this)
        setContentView(layout())

        if (Build.VERSION.SDK_INT >= 33) {
            onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT) { back() }
        }
        if (!openIntent(intent)) newTab(Rules.HOME, null)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        openIntent(intent)
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
        gecko.releaseSession()
        tabs.forEach { it.session.close() }
        super.onDestroy()
    }

    @Deprecated("Deprecated in Activity")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (requestCode == PICK_FILES) {
            prompts.filesPicked(if (resultCode == RESULT_OK) data else null)
        } else {
            @Suppress("DEPRECATION")
            super.onActivityResult(requestCode, resultCode, data)
        }
    }

    @Deprecated("Android 12 and older; newer versions use the callback from onCreate")
    override fun onBackPressed() = back()

    /** A .ov link from another app. */
    private fun openIntent(intent: Intent?): Boolean {
        if (intent?.action != Intent.ACTION_VIEW) return false
        val url = intent.dataString ?: return false
        if (!url.startsWith("http://") && !url.startsWith("https://")) return false
        newTab(Rules.redirect(url) ?: url, null)
        return true
    }

    private fun back() {
        val tab = current
        when {
            panel.visibility == View.VISIBLE -> hidePanel()
            tab?.fullScreen == true -> tab.session.exitFullScreen()
            address.hasFocus() -> address.clearFocus()
            tab?.canGoBack == true -> tab.session.goBack()
            // A tab a link opened goes away, back to the page with the link.
            tab?.parent != null && tabs.contains(tab.parent) -> closeTab(tab)
            else -> moveTaskToBack(true)
        }
    }

    // ---- Tabs ----

    /**
     * A new tab. [url] null: Gecko opens the session itself and loads what the
     * page asked for (onNewSession), so it must not be opened here.
     */
    private fun newTab(url: String?, parent: Tab?, select: Boolean = true): Tab {
        val session = GeckoSession(GeckoSessionSettings.Builder().build())
        val tab = Tab(session, parent)
        session.navigationDelegate = Navigation(tab)
        session.progressDelegate = Progress(tab)
        session.contentDelegate = Content(tab)
        session.promptDelegate = prompts
        val at = parent?.let { p -> tabs.indexOf(p).takeIf { it >= 0 }?.let { it + 1 + tabs.drop(it + 1).takeWhile { t -> t.parent == p }.size } }
        tabs.add(at ?: tabs.size, tab)
        if (url != null) {
            session.open(runtime)
            tab.url = url
            session.loadUri(url)
        }
        if (select) selectTab(tab)
        updateTabCount()
        return tab
    }

    private fun selectTab(tab: Tab) {
        // A session from onNewSession: Gecko opens it right after we return it.
        if (!tab.session.isOpen) {
            handler.postDelayed({ if (tabs.contains(tab)) selectTab(tab) }, 30)
            return
        }
        if (current === tab && gecko.session === tab.session) return
        current?.let { capture(it) }
        current?.session?.setActive(false)
        gecko.releaseSession()
        gecko.setSession(tab.session)
        tab.session.setActive(true)
        current = tab
        address.clearFocus()
        showTab()
    }

    private fun closeTab(tab: Tab) {
        val i = tabs.indexOf(tab)
        if (i < 0) return
        tabs.removeAt(i)
        tabs.forEach { if (it.parent === tab) it.parent = tab.parent }
        if (current === tab) {
            gecko.releaseSession()
            current = null
            val next = tab.parent?.takeIf { tabs.contains(it) } ?: tabs.getOrNull(i) ?: tabs.getOrNull(i - 1)
            if (next != null) selectTab(next) else newTab(Rules.HOME, null)
        }
        tab.session.close()
        updateTabCount()
        if (panel.visibility == View.VISIBLE) fillPanel()
    }

    private fun closeAll() {
        val all = tabs.toList()
        tabs.clear()
        gecko.releaseSession()
        current = null
        all.forEach { it.session.close() }
        newTab(Rules.HOME, null)
        hidePanel()
    }

    // ---- Gecko's delegates, one set per tab ----

    private inner class Navigation(val tab: Tab) : NavigationDelegate {
        override fun onLocationChange(
            session: GeckoSession,
            url: String?,
            perms: MutableList<PermissionDelegate.ContentPermission>,
            hasUserGesture: Boolean,
        ) {
            tab.url = url ?: ""
            if (tab === current) showTab()
        }

        override fun onCanGoBack(session: GeckoSession, canGoBack: Boolean) {
            tab.canGoBack = canGoBack
        }

        override fun onCanGoForward(session: GeckoSession, canGoForward: Boolean) {
            tab.canGoForward = canGoForward
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
            Rules.openTarget(uri, Engine.status().token)?.let { target ->
                if (target.isNotEmpty()) handler.post { openExternal(target) }
                return GeckoResult.deny()
            }
            val next = Rules.redirect(request.uri) ?: return GeckoResult.allow()
            handler.post {
                if (request.target == NavigationDelegate.TARGET_WINDOW_NEW) newTab(next, tab) else session.loadUri(next)
            }
            return GeckoResult.deny()
        }

        override fun onNewSession(session: GeckoSession, uri: String): GeckoResult<GeckoSession> {
            val t = newTab(null, tab)
            t.url = uri
            return GeckoResult.fromValue(t.session)
        }
    }

    private inner class Progress(val tab: Tab) : ProgressDelegate {
        override fun onPageStart(session: GeckoSession, url: String) {
            tab.loading = true
            tab.progress = 5
            if (tab === current) showProgress()
        }

        override fun onProgressChange(session: GeckoSession, progress: Int) {
            tab.progress = progress
            if (tab === current) showProgress()
        }

        override fun onPageStop(session: GeckoSession, success: Boolean) {
            tab.loading = false
            tab.progress = 100
            if (tab === current) {
                showProgress()
                // A card for the tab list while the page is on screen (after a
                // switch the capture would be too late).
                handler.postDelayed({ if (tab === current) capture(tab) }, 700)
            }
        }
    }

    private inner class Content(val tab: Tab) : ContentDelegate {
        override fun onTitleChange(session: GeckoSession, title: String?) {
            tab.title = title ?: ""
        }

        override fun onCloseRequest(session: GeckoSession) = closeTab(tab)

        override fun onFullScreen(session: GeckoSession, fullScreen: Boolean) {
            tab.fullScreen = fullScreen
            if (tab === current) showFullScreen(fullScreen)
        }

        override fun onContextMenu(session: GeckoSession, screenX: Int, screenY: Int, element: ContentDelegate.ContextElement) {
            contextMenu(tab, element)
        }

        override fun onExternalResponse(session: GeckoSession, response: WebResponse) = download(response)

        override fun onCrash(session: GeckoSession) = revive(tab, true)

        override fun onKill(session: GeckoSession) = revive(tab, false)
    }

    // Gecko's content process died: under memory pressure (kill) or a crash.
    private fun revive(tab: Tab, crashed: Boolean) {
        if (!tab.session.isOpen) tab.session.open(runtime)
        if (tab === current) {
            gecko.releaseSession()
            gecko.setSession(tab.session)
        }
        tab.session.loadUri(tab.url.ifEmpty { Rules.HOME })
        if (crashed) snack(getString(R.string.tab_crashed))
    }

    // ---- Address bar ----

    private fun showTab() {
        val tab = current ?: return
        if (!address.hasFocus()) address.setText(Rules.display(tab.url))
        showProgress()
        showFullScreen(tab.fullScreen)
    }

    private fun showProgress() {
        val tab = current ?: return
        progress.set(if (tab.loading) tab.progress else 100)
        if (!address.hasFocus()) {
            actionGlyph = Glyph(if (tab.loading) Glyph.Kind.CLOSE else Glyph.Kind.RELOAD, getColor(R.color.muted))
            action.setImageDrawable(actionGlyph)
            action.contentDescription = getString(if (tab.loading) R.string.stop else R.string.reload)
        }
    }

    private fun go(typed: String) {
        val url = Rules.fromTyped(typed) ?: return
        val tab = current
        if (tab == null) {
            newTab(url, null)
        } else {
            tab.url = url
            tab.session.loadUri(url)
        }
        address.clearFocus()
    }

    private fun hideKeyboard() {
        getSystemService(InputMethodManager::class.java).hideSoftInputFromWindow(address.windowToken, 0)
    }

    private fun updateTabCount() {
        tabCount.text = if (tabs.size > 99) "∞" else tabs.size.toString()
    }

    private fun showStatus() {
        val s = Engine.status()
        dot.color = getColor(
            when {
                !s.running -> R.color.err
                s.relays > 0 -> R.color.ok
                else -> R.color.warn
            },
        )
    }

    private fun statusText(s: Status) = when {
        !s.running -> getString(R.string.status_failed, s.error)
        s.relays > 0 -> getString(R.string.status_connected, s.relays)
        else -> getString(R.string.status_connecting)
    }

    private fun showFullScreen(on: Boolean) {
        bar.visibility = if (on) View.GONE else View.VISIBLE
        progress.visibility = if (on) View.GONE else View.VISIBLE
        if (Build.VERSION.SDK_INT >= 30) {
            val c = window.insetsController ?: return
            if (on) {
                c.hide(WindowInsets.Type.systemBars())
                c.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            } else {
                c.show(WindowInsets.Type.systemBars())
            }
        } else {
            @Suppress("DEPRECATION")
            window.decorView.systemUiVisibility = if (on) {
                View.SYSTEM_UI_FLAG_FULLSCREEN or View.SYSTEM_UI_FLAG_HIDE_NAVIGATION or View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
            } else {
                0
            }
        }
        top.requestApplyInsets()
    }

    // ---- Actions ----

    private fun openExternal(url: String) {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addCategory(Intent.CATEGORY_BROWSABLE))
        } catch (e: ActivityNotFoundException) {
            snack(getString(R.string.no_app))
        }
    }

    private fun share(url: String) {
        val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, url)
        startActivity(Intent.createChooser(send, null))
    }

    private fun copy(url: String) {
        getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("URL", url))
        // Android 13+ shows its own confirmation.
        if (Build.VERSION.SDK_INT < 33) snack(getString(R.string.link_copied))
    }

    private fun newCircuits() {
        Gateway.newCircuits()
        snack(getString(R.string.new_circuits_done))
    }

    private fun download(response: WebResponse) {
        snack(getString(R.string.download_started, Downloads.name(response)))
        Downloads.save(this, response) { saved, error ->
            when {
                saved == null -> snack(getString(R.string.download_failed, error))
                saved.uri != null -> snack(getString(R.string.download_done, saved.name), getString(R.string.open)) {
                    try {
                        startActivity(
                            Intent(Intent.ACTION_VIEW).setDataAndType(saved.uri, saved.mime)
                                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION),
                        )
                    } catch (e: ActivityNotFoundException) {
                        snack(getString(R.string.no_app))
                    }
                }
                else -> snack(getString(R.string.download_saved_app, saved.name))
            }
        }
    }

    // ---- Layout ----

    fun dp(v: Int) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v.toFloat(), resources.displayMetrics).toInt()

    private fun rounded(fill: Int, radius: Int, strokeColor: Int? = null, strokeWidth: Int = 1) = GradientDrawable().apply {
        setColor(fill)
        cornerRadius = dp(radius).toFloat()
        if (strokeColor != null) setStroke(dp(strokeWidth), strokeColor)
    }

    private fun ripple(v: View, borderless: Boolean = false) {
        val tv = TypedValue()
        theme.resolveAttribute(
            if (borderless) android.R.attr.selectableItemBackgroundBorderless else android.R.attr.selectableItemBackground,
            tv,
            true,
        )
        v.foreground = getDrawable(tv.resourceId)
    }

    private fun iconButton(kind: Glyph.Kind, label: Int, onClick: () -> Unit) = ImageView(this).apply {
        setImageDrawable(Glyph(kind, getColor(R.color.text_2)))
        contentDescription = getString(label)
        setPadding(dp(11), dp(11), dp(11), dp(11))
        ripple(this, true)
        setOnClickListener { onClick() }
    }

    private fun text(size: Float, color: Int, medium: Boolean = false) = TextView(this).apply {
        textSize = size
        setTextColor(getColor(color))
        typeface = if (medium) Typeface.create("sans-serif-medium", Typeface.NORMAL) else Typeface.DEFAULT
    }

    /** A text field for the prompts, on the surface color. */
    fun field(value: String, hint: String?) = EditText(this).apply {
        setText(value)
        this.hint = hint
        setTextColor(getColor(R.color.text))
        setHintTextColor(getColor(R.color.faint))
        textSize = 16f
        isSingleLine = true
        background = rounded(getColor(R.color.surface_2), 8)
        setPadding(dp(12), dp(10), dp(12), dp(10))
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            .apply { topMargin = dp(8) }
    }

    fun padded(v: View) = FrameLayout(this).apply {
        setPadding(dp(20), dp(4), dp(20), 0)
        addView(v)
    }

    private fun layout(): View {
        dot = Glyph(Glyph.Kind.NETWORK, getColor(R.color.warn))
        val dotView = ImageView(this).apply {
            setImageDrawable(dot)
            contentDescription = getString(R.string.network)
            setPadding(dp(12), dp(12), dp(6), dp(12))
            setOnClickListener { networkSheet() }
        }
        address = EditText(this).apply {
            hint = getString(R.string.address_hint)
            setHintTextColor(getColor(R.color.muted))
            setTextColor(getColor(R.color.text))
            textSize = 15f
            isSingleLine = true
            ellipsize = TextUtils.TruncateAt.END
            background = null
            imeOptions = EditorInfo.IME_ACTION_GO or EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
            inputType = EditorInfo.TYPE_CLASS_TEXT or EditorInfo.TYPE_TEXT_VARIATION_URI
            setPadding(dp(4), 0, dp(4), 0)
            setSelectAllOnFocus(true)
            setOnFocusChangeListener { _, focused ->
                val tab = current
                if (focused) {
                    setText(tab?.url?.takeUnless { it == Rules.HOME || it == "about:blank" } ?: "")
                    selectAll()
                    actionGlyph = Glyph(Glyph.Kind.CLOSE, getColor(R.color.muted))
                    action.setImageDrawable(actionGlyph)
                    action.contentDescription = getString(R.string.close)
                } else {
                    hideKeyboard()
                    showTab()
                }
            }
            setOnEditorActionListener { v, act, event ->
                val enter = event?.keyCode == KeyEvent.KEYCODE_ENTER && event.action == KeyEvent.ACTION_DOWN
                if (act == EditorInfo.IME_ACTION_GO || enter) {
                    go(v.text.toString())
                    true
                } else {
                    false
                }
            }
        }
        actionGlyph = Glyph(Glyph.Kind.RELOAD, getColor(R.color.muted))
        action = ImageView(this).apply {
            setImageDrawable(actionGlyph)
            setPadding(dp(11), dp(11), dp(11), dp(11))
            ripple(this, true)
            setOnClickListener {
                val tab = current
                when {
                    address.hasFocus() -> address.setText("")
                    tab == null -> {}
                    tab.loading -> tab.session.stop()
                    else -> tab.session.reload()
                }
            }
        }
        val pill = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            background = rounded(getColor(R.color.surface_2), 10)
            addView(dotView, LinearLayout.LayoutParams(dp(40), dp(44)))
            addView(address, LinearLayout.LayoutParams(0, dp(44), 1f))
            addView(action, LinearLayout.LayoutParams(dp(42), dp(44)))
        }
        tabCount = text(13f, R.color.text_2, medium = true).apply {
            gravity = Gravity.CENTER
            background = rounded(0, 5, getColor(R.color.text_2), 2)
        }
        val tabsButton = FrameLayout(this).apply {
            contentDescription = getString(R.string.tabs)
            ripple(this, true)
            addView(tabCount, FrameLayout.LayoutParams(dp(22), dp(22), Gravity.CENTER))
            setOnClickListener { showPanel() }
        }
        val menuButton = iconButton(Glyph.Kind.MENU, R.string.app_name) { menuSheet() }
        bar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setBackgroundColor(getColor(R.color.bg))
            setPadding(dp(8), dp(6), dp(2), dp(6))
            addView(pill, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            addView(tabsButton, LinearLayout.LayoutParams(dp(46), dp(46)).apply { leftMargin = dp(4) })
            addView(menuButton, LinearLayout.LayoutParams(dp(46), dp(46)))
        }
        progress = ProgressLine(this)

        snack = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            background = rounded(getColor(R.color.surface), 8, getColor(R.color.line))
            elevation = dp(6).toFloat()
            setPadding(dp(14), dp(4), dp(4), dp(4))
            visibility = View.GONE
        }
        val content = FrameLayout(this).apply {
            addView(gecko, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
            addView(
                snack,
                FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM)
                    .apply { setMargins(dp(10), 0, dp(10), dp(10)) },
            )
        }
        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(content, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
            addView(progress, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(2)))
            addView(bar, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        }
        panel = FrameLayout(this).apply {
            setBackgroundColor(getColor(R.color.bg))
            visibility = View.GONE
            isClickable = true
        }
        top = FrameLayout(this).apply {
            setBackgroundColor(getColor(R.color.bg))
            addView(column)
            addView(panel)
        }
        // Android 15+ draws apps under the system bars and the keyboard.
        if (Build.VERSION.SDK_INT >= 35) {
            top.setOnApplyWindowInsetsListener { v, insets ->
                val full = current?.fullScreen == true
                val b = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.ime() or WindowInsets.Type.displayCutout())
                if (full) v.setPadding(0, 0, 0, 0) else v.setPadding(b.left, b.top, b.right, b.bottom)
                WindowInsets.CONSUMED
            }
        } else {
            window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
        }
        return top
    }

    // ---- The little message above the bar ----

    private val hideSnack = Runnable { snack.visibility = View.GONE }

    private fun snack(message: String, actionLabel: String? = null, onAction: (() -> Unit)? = null) {
        snack.removeAllViews()
        snack.addView(
            text(14f, R.color.text_2).apply {
                text = message
                maxLines = 2
                ellipsize = TextUtils.TruncateAt.MIDDLE
                setPadding(0, dp(10), dp(10), dp(10))
            },
            LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
        )
        if (actionLabel != null && onAction != null) {
            snack.addView(
                text(14f, R.color.accent_text, medium = true).apply {
                    text = actionLabel
                    setPadding(dp(12), dp(10), dp(12), dp(10))
                    ripple(this)
                    setOnClickListener {
                        snack.visibility = View.GONE
                        onAction()
                    }
                },
            )
        }
        snack.visibility = View.VISIBLE
        handler.removeCallbacks(hideSnack)
        handler.postDelayed(hideSnack, if (actionLabel != null) 6000 else 3000)
    }

    // ---- Sheets: the menu, the long-press menu, the network ----

    private fun sheet(build: (LinearLayout, Dialog) -> Unit) {
        val d = Dialog(this, R.style.Overnet_Sheet)
        val box = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = GradientDrawable().apply {
                setColor(getColor(R.color.surface))
                val r = dp(14).toFloat()
                cornerRadii = floatArrayOf(r, r, r, r, 0f, 0f, 0f, 0f)
            }
            setPadding(0, dp(8), 0, dp(10))
        }
        build(box, d)
        d.setContentView(box)
        d.window?.apply {
            setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            setGravity(Gravity.BOTTOM)
            if (Build.VERSION.SDK_INT >= 35) {
                decorView.setOnApplyWindowInsetsListener { _, insets ->
                    val b = insets.getInsets(WindowInsets.Type.navigationBars())
                    box.setPadding(0, dp(8), 0, dp(10) + b.bottom)
                    insets
                }
            }
        }
        d.show()
    }

    private fun sheetItem(box: LinearLayout, d: Dialog, kind: Glyph.Kind, label: String, sub: String? = null, subColor: Int = R.color.muted, onClick: () -> Unit) {
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(20), dp(4), dp(20), dp(4))
            minimumHeight = dp(50)
            ripple(this)
            setOnClickListener {
                d.dismiss()
                onClick()
            }
        }
        row.addView(ImageView(this).apply { setImageDrawable(Glyph(kind, getColor(R.color.text_2))) }, LinearLayout.LayoutParams(dp(22), dp(22)))
        val texts = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        texts.addView(text(16f, R.color.text).apply { text = label })
        if (sub != null) texts.addView(text(13f, subColor).apply { text = sub })
        row.addView(texts, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply { leftMargin = dp(18) })
        box.addView(row)
    }

    private fun sheetHeader(box: LinearLayout, title: String) {
        box.addView(
            text(13f, R.color.muted).apply {
                text = title
                maxLines = 2
                ellipsize = TextUtils.TruncateAt.MIDDLE
                setPadding(dp(20), dp(8), dp(20), dp(10))
            },
        )
    }

    private fun menuSheet() {
        val tab = current ?: return
        sheet { box, d ->
            val row = LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                setPadding(dp(8), 0, dp(8), dp(6))
            }
            fun top(kind: Glyph.Kind, label: Int, enabled: Boolean, onClick: () -> Unit) {
                row.addView(
                    iconButton(kind, label) {
                        d.dismiss()
                        onClick()
                    }.apply {
                        isEnabled = enabled
                        alpha = if (enabled) 1f else 0.4f
                    },
                    LinearLayout.LayoutParams(0, dp(52), 1f),
                )
            }
            top(Glyph.Kind.BACK, R.string.back, tab.canGoBack) { tab.session.goBack() }
            top(Glyph.Kind.FORWARD, R.string.forward, tab.canGoForward) { tab.session.goForward() }
            if (tab.loading) {
                top(Glyph.Kind.CLOSE, R.string.stop, true) { tab.session.stop() }
            } else {
                top(Glyph.Kind.RELOAD, R.string.reload, true) { tab.session.reload() }
            }
            top(Glyph.Kind.SHARE, R.string.share, tab.url.startsWith("http")) { share(tab.url) }
            box.addView(row)
            box.addView(View(this).apply { setBackgroundColor(getColor(R.color.line_soft)) }, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 1))

            sheetItem(box, d, Glyph.Kind.PLUS, getString(R.string.new_tab)) { newTab(Rules.HOME, null) }
            sheetItem(box, d, Glyph.Kind.HOME, getString(R.string.home)) { tab.session.loadUri(Rules.HOME) }
            if (tab.url.startsWith("http")) {
                sheetItem(box, d, Glyph.Kind.COPY, getString(R.string.copy_link)) { copy(tab.url) }
            }
            sheetItem(box, d, Glyph.Kind.CIRCUITS, getString(R.string.new_circuits)) { newCircuits() }
            val s = Engine.status()
            val color = when {
                !s.running -> R.color.err
                s.relays > 0 -> R.color.ok
                else -> R.color.warn
            }
            sheetItem(box, d, Glyph.Kind.NETWORK, getString(R.string.network), statusText(s), color) { networkSheet() }
        }
    }

    private fun contextMenu(tab: Tab, e: ContentDelegate.ContextElement) {
        val link = e.linkUri
        val src = e.srcUri.takeIf { e.type == ContentDelegate.ContextElement.TYPE_IMAGE }
        if (link == null && src == null) return
        sheet { box, d ->
            sheetHeader(box, Rules.display(link ?: src!!).ifEmpty { link ?: src!! })
            if (link != null) {
                sheetItem(box, d, Glyph.Kind.PLUS, getString(R.string.open_in_new_tab)) {
                    val opened = newTab(Rules.redirect(link) ?: link, tab, select = false)
                    snack(getString(R.string.new_tab), getString(R.string.open)) { if (tabs.contains(opened)) selectTab(opened) }
                }
                sheetItem(box, d, Glyph.Kind.COPY, getString(R.string.copy_link)) { copy(link) }
                sheetItem(box, d, Glyph.Kind.SHARE, getString(R.string.share_link)) { share(link) }
            }
            if (src != null) {
                sheetItem(box, d, Glyph.Kind.IMAGE, getString(R.string.open_image)) { newTab(Rules.redirect(src) ?: src, tab) }
                sheetItem(box, d, Glyph.Kind.LINK, getString(R.string.copy_image_link)) { copy(src) }
            }
        }
    }

    private fun networkSheet() {
        val s = Engine.status()
        AlertDialog.Builder(this)
            .setTitle(R.string.network)
            .setMessage(statusText(s) + "\n\n" + getString(R.string.status_regular_sites))
            .setPositiveButton(R.string.new_circuits) { _, _ -> newCircuits() }
            .setNegativeButton(R.string.close, null)
            .show()
    }

    // ---- The tab list ----

    private fun showPanel() {
        address.clearFocus()
        val tab = current
        fillPanel()
        panel.visibility = View.VISIBLE
        if (tab != null) capture(tab)
    }

    /** The page as it looks now, for its card in the tab list. */
    private fun capture(tab: Tab) {
        if (gecko.session !== tab.session) return
        gecko.capturePixels().accept({ bmp ->
            if (bmp != null) {
                tab.thumbnail = thumbnail(bmp)
                if (panel.visibility == View.VISIBLE) fillPanel()
            }
        }, { })
    }

    private fun hidePanel() {
        panel.visibility = View.GONE
    }

    // The top of the page, small: a card shows no more than that.
    private fun thumbnail(src: Bitmap): Bitmap {
        val w = src.width
        val h = minOf(src.height, (w * 1.25f).toInt())
        val scale = minOf(1f, 360f / w)
        val out = Bitmap.createBitmap((w * scale).toInt().coerceAtLeast(1), (h * scale).toInt().coerceAtLeast(1), Bitmap.Config.RGB_565)
        Canvas(out).apply {
            scale(scale, scale)
            drawBitmap(src, 0f, 0f, Paint(Paint.FILTER_BITMAP_FLAG))
        }
        return out
    }

    private fun fillPanel() {
        panel.removeAllViews()
        val column = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }

        val header = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(16), dp(8), dp(4), dp(8))
        }
        header.addView(iconButton(Glyph.Kind.BACK, R.string.back) { hidePanel() }, LinearLayout.LayoutParams(dp(44), dp(44)))
        header.addView(
            text(20f, R.color.text, medium = true).apply {
                text = getString(R.string.tabs) + "  "
                append(android.text.SpannableString(tabs.size.toString()).apply {
                    setSpan(android.text.style.ForegroundColorSpan(getColor(R.color.muted)), 0, length, 0)
                })
                setPadding(dp(8), 0, 0, 0)
            },
            LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
        )
        header.addView(
            text(14f, R.color.accent_text, medium = true).apply {
                text = getString(R.string.close_all)
                setPadding(dp(14), dp(10), dp(14), dp(10))
                ripple(this)
                setOnClickListener { closeAll() }
            },
        )
        column.addView(header)

        val grid = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(12), 0, dp(12), dp(12))
        }
        tabs.chunked(2).forEach { pair ->
            val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
            pair.forEach { row.addView(card(it), LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply { setMargins(dp(4), dp(4), dp(4), dp(4)) }) }
            if (pair.size == 1) row.addView(View(this), LinearLayout.LayoutParams(0, 1, 1f).apply { setMargins(dp(4), 0, dp(4), 0) })
            grid.addView(row)
        }
        val scroll = ScrollView(this).apply { addView(grid) }
        column.addView(scroll, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))

        val newButton = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER
            background = rounded(0, 8, getColor(R.color.accent))
            ripple(this)
            addView(ImageView(this@MainActivity).apply { setImageDrawable(Glyph(Glyph.Kind.PLUS, getColor(R.color.accent_text))) }, LinearLayout.LayoutParams(dp(20), dp(20)))
            addView(text(15f, R.color.accent_text, medium = true).apply {
                text = getString(R.string.new_tab)
                setPadding(dp(10), 0, 0, 0)
            })
            setOnClickListener {
                newTab(Rules.HOME, null)
                hidePanel()
            }
        }
        column.addView(newButton, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(48)).apply { setMargins(dp(16), dp(4), dp(16), dp(12)) })
        panel.addView(column)
        // Keep the current tab in view.
        val i = tabs.indexOf(current)
        if (i >= 2) scroll.post { grid.getChildAt(i / 2)?.let { scroll.scrollTo(0, it.top) } }
    }

    private fun card(tab: Tab): View {
        val selected = tab === current
        val card = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = rounded(getColor(R.color.surface), 8, getColor(if (selected) R.color.accent else R.color.line_soft), if (selected) 2 else 1)
            clipToOutline = true
            val inset = dp(if (selected) 2 else 1)
            setPadding(inset, 0, inset, inset)
            ripple(this)
            setOnClickListener {
                selectTab(tab)
                hidePanel()
            }
        }
        val head = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(10), 0, 0, 0)
        }
        val title = tab.title.ifEmpty { Rules.display(tab.url).ifEmpty { getString(R.string.new_tab_title) } }
        head.addView(
            text(13f, R.color.text_2).apply {
                text = title
                maxLines = 1
                ellipsize = TextUtils.TruncateAt.END
            },
            LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
        )
        head.addView(
            ImageView(this).apply {
                setImageDrawable(Glyph(Glyph.Kind.CLOSE, getColor(R.color.muted)))
                contentDescription = getString(R.string.close_tab)
                setPadding(dp(10), dp(10), dp(10), dp(10))
                ripple(this, true)
                setOnClickListener { closeTab(tab) }
            },
            LinearLayout.LayoutParams(dp(38), dp(38)),
        )
        card.addView(head)
        val thumb = tab.thumbnail
        val preview: View = if (thumb != null) {
            // The top of the page, as wide as the card.
            ImageView(this).apply {
                setImageBitmap(thumb)
                scaleType = ImageView.ScaleType.MATRIX
                addOnLayoutChangeListener { v, l, _, r, _, _, _, _, _ ->
                    val k = (r - l).toFloat() / thumb.width
                    (v as ImageView).imageMatrix = android.graphics.Matrix().apply { setScale(k, k) }
                }
            }
        } else {
            FrameLayout(this).apply {
                setBackgroundColor(getColor(R.color.bg))
                addView(
                    ImageView(this@MainActivity).apply { setImageDrawable(Glyph(Glyph.Kind.NETWORK, getColor(R.color.line))) },
                    FrameLayout.LayoutParams(dp(36), dp(36), Gravity.CENTER),
                )
            }
        }
        card.addView(preview, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(170)))
        return card
    }

    /** The thin loading line above the address bar. */
    private class ProgressLine(context: Context) : View(context) {
        private val paint = Paint().apply { color = context.getColor(R.color.accent) }
        private var value = 100

        fun set(v: Int) {
            value = v
            invalidate()
        }

        override fun onDraw(canvas: Canvas) {
            canvas.drawColor(context.getColor(R.color.line_soft))
            if (value in 1..99) canvas.drawRect(0f, 0f, width * value / 100f, height.toFloat(), paint)
        }
    }
}
