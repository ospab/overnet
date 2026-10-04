package com.ospab.overnet

import android.graphics.Bitmap
import org.mozilla.geckoview.GeckoSession

/** One open page: its Gecko session and what the interface shows about it. */
class Tab(val session: GeckoSession, var parent: Tab?) {
    var url = ""
    var title = ""
    var progress = 0
    var loading = false
    var canGoBack = false
    var canGoForward = false
    var fullScreen = false
    /** The page as it last looked, for the tab list. */
    var thumbnail: Bitmap? = null
}
