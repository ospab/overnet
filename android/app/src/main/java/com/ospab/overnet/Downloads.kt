package com.ospab.overnet

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.Handler
import android.os.Looper
import android.provider.MediaStore
import android.webkit.MimeTypeMap
import org.mozilla.geckoview.WebResponse
import java.io.File
import java.net.URLDecoder

/**
 * A file a page hands over instead of showing it. It has already come through
 * the gateway, so saving it is all that's left: to the system's Downloads from
 * Android 10 on, to the app's own folder before that (no storage permission).
 */
object Downloads {
    class Saved(val name: String, val uri: Uri?, val mime: String, val public: Boolean)

    fun name(response: WebResponse): String {
        val header = response.headers.entries.firstOrNull { it.key.equals("content-disposition", true) }?.value
        var name = header?.let { fromDisposition(it) }
            ?: Uri.parse(response.uri).lastPathSegment
            ?: "download"
        name = name.replace(Regex("[\\\\/:*?\"<>|\\x00-\\x1f]"), "_").trim().trimStart('.')
        if (name.isEmpty()) name = "download"
        if (!name.contains('.')) {
            MimeTypeMap.getSingleton().getExtensionFromMimeType(mime(response))?.let { name += ".$it" }
        }
        return name.take(120)
    }

    /** Saves on a background thread; [done] gets the file or the error on the main thread. */
    fun save(context: Context, response: WebResponse, done: (Saved?, String?) -> Unit) {
        val app = context.applicationContext
        val main = Handler(Looper.getMainLooper())
        val name = name(response)
        val mime = mime(response)
        Thread {
            val result = runCatching {
                val body = response.body ?: error("empty response")
                body.use { input ->
                    if (Build.VERSION.SDK_INT >= 29) {
                        val values = ContentValues().apply {
                            put(MediaStore.Downloads.DISPLAY_NAME, name)
                            put(MediaStore.Downloads.MIME_TYPE, mime)
                            put(MediaStore.Downloads.IS_PENDING, 1)
                        }
                        val resolver = app.contentResolver
                        val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                            ?: error("no Downloads folder")
                        try {
                            resolver.openOutputStream(uri)!!.use { input.copyTo(it) }
                            values.clear()
                            values.put(MediaStore.Downloads.IS_PENDING, 0)
                            resolver.update(uri, values, null, null)
                        } catch (e: Throwable) {
                            resolver.delete(uri, null, null)
                            throw e
                        }
                        Saved(name, uri, mime, true)
                    } else {
                        val dir = app.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: File(app.filesDir, "downloads")
                        dir.mkdirs()
                        var file = File(dir, name)
                        var n = 1
                        while (file.exists()) file = File(dir, "${name.substringBeforeLast('.')} (${n++}).${name.substringAfterLast('.', "")}".removeSuffix("."))
                        file.outputStream().use { input.copyTo(it) }
                        Saved(file.name, null, mime, false)
                    }
                }
            }
            main.post {
                result.onSuccess { done(it, null) }
                    .onFailure { done(null, it.message ?: it.javaClass.simpleName) }
            }
        }.start()
    }

    private fun mime(response: WebResponse): String {
        val type = response.headers.entries.firstOrNull { it.key.equals("content-type", true) }?.value
        return type?.substringBefore(';')?.trim()?.lowercase()?.takeIf { it.contains('/') }
            ?: "application/octet-stream"
    }

    // filename*=UTF-8''name.pdf or filename="name.pdf"
    private fun fromDisposition(h: String): String? {
        Regex("filename\\*\\s*=\\s*([^']*)''([^;]+)", RegexOption.IGNORE_CASE).find(h)?.let {
            return runCatching { URLDecoder.decode(it.groupValues[2].trim(), it.groupValues[1].ifEmpty { "UTF-8" }) }.getOrNull()
        }
        Regex("filename\\s*=\\s*\"?([^\";]+)\"?", RegexOption.IGNORE_CASE).find(h)?.let {
            return it.groupValues[1].trim()
        }
        return null
    }
}
