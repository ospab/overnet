package com.ospab.overnet

import android.app.AlertDialog
import android.content.Intent
import android.net.Uri
import android.text.InputType
import android.widget.EditText
import android.widget.LinearLayout
import org.mozilla.geckoview.AllowOrDeny
import org.mozilla.geckoview.GeckoResult
import org.mozilla.geckoview.GeckoSession
import org.mozilla.geckoview.GeckoSession.PromptDelegate
import org.mozilla.geckoview.GeckoSession.PromptDelegate.AlertPrompt
import org.mozilla.geckoview.GeckoSession.PromptDelegate.AuthPrompt
import org.mozilla.geckoview.GeckoSession.PromptDelegate.BeforeUnloadPrompt
import org.mozilla.geckoview.GeckoSession.PromptDelegate.ButtonPrompt
import org.mozilla.geckoview.GeckoSession.PromptDelegate.ChoicePrompt
import org.mozilla.geckoview.GeckoSession.PromptDelegate.FilePrompt
import org.mozilla.geckoview.GeckoSession.PromptDelegate.PromptResponse
import org.mozilla.geckoview.GeckoSession.PromptDelegate.RepostConfirmPrompt
import org.mozilla.geckoview.GeckoSession.PromptDelegate.TextPrompt

/**
 * What a page asks the user: alert/confirm/prompt, <select>, sign-in, file
 * picking, "leave this page?". GeckoView draws none of it itself.
 */
class Prompts(private val activity: MainActivity) : PromptDelegate {
    private var pendingFiles: Pair<FilePrompt, GeckoResult<PromptResponse>>? = null

    private fun dialog() = AlertDialog.Builder(activity)

    override fun onAlertPrompt(session: GeckoSession, prompt: AlertPrompt): GeckoResult<PromptResponse> {
        val r = GeckoResult<PromptResponse>()
        dialog().setTitle(prompt.title).setMessage(prompt.message)
            .setPositiveButton(R.string.ok, null)
            .setOnDismissListener { r.complete(prompt.dismiss()) }
            .show()
        return r
    }

    override fun onButtonPrompt(session: GeckoSession, prompt: ButtonPrompt): GeckoResult<PromptResponse> {
        val r = GeckoResult<PromptResponse>()
        var answer: PromptResponse? = null
        dialog().setTitle(prompt.title).setMessage(prompt.message)
            .setPositiveButton(R.string.ok) { _, _ -> answer = prompt.confirm(ButtonPrompt.Type.POSITIVE) }
            .setNegativeButton(R.string.cancel) { _, _ -> answer = prompt.confirm(ButtonPrompt.Type.NEGATIVE) }
            .setOnDismissListener { r.complete(answer ?: prompt.dismiss()) }
            .show()
        return r
    }

    override fun onTextPrompt(session: GeckoSession, prompt: TextPrompt): GeckoResult<PromptResponse> {
        val r = GeckoResult<PromptResponse>()
        var answer: PromptResponse? = null
        val field = activity.field(prompt.defaultValue ?: "", null)
        dialog().setTitle(prompt.title).setMessage(prompt.message).setView(activity.padded(field))
            .setPositiveButton(R.string.ok) { _, _ -> answer = prompt.confirm(field.text.toString()) }
            .setNegativeButton(R.string.cancel, null)
            .setOnDismissListener { r.complete(answer ?: prompt.dismiss()) }
            .show()
        return r
    }

    override fun onAuthPrompt(session: GeckoSession, prompt: AuthPrompt): GeckoResult<PromptResponse> {
        val r = GeckoResult<PromptResponse>()
        var answer: PromptResponse? = null
        val onlyPassword = prompt.authOptions.flags and AuthPrompt.AuthOptions.Flags.ONLY_PASSWORD != 0
        val user = activity.field(prompt.authOptions.username ?: "", activity.getString(R.string.username))
        val pass = activity.field(prompt.authOptions.password ?: "", activity.getString(R.string.password))
        pass.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
        val box = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            if (!onlyPassword) addView(user)
            addView(pass)
        }
        dialog().setTitle(prompt.title ?: activity.getString(R.string.sign_in)).setMessage(prompt.message)
            .setView(activity.padded(box))
            .setPositiveButton(R.string.sign_in) { _, _ ->
                answer = if (onlyPassword) prompt.confirm(pass.text.toString())
                else prompt.confirm(user.text.toString(), pass.text.toString())
            }
            .setNegativeButton(R.string.cancel, null)
            .setOnDismissListener { r.complete(answer ?: prompt.dismiss()) }
            .show()
        return r
    }

    override fun onChoicePrompt(session: GeckoSession, prompt: ChoicePrompt): GeckoResult<PromptResponse> {
        val r = GeckoResult<PromptResponse>()
        var answer: PromptResponse? = null
        // Groups (<optgroup>) flattened: the group's label, then its options.
        val flat = ArrayList<ChoicePrompt.Choice>()
        fun add(list: Array<ChoicePrompt.Choice>) {
            for (c in list) {
                if (c.separator) continue
                flat.add(c)
                c.items?.let { add(it) }
            }
        }
        add(prompt.choices)
        val choices = flat.filter { it.items == null }
        val labels = flat.filter { it.items == null }.map { it.label }.toTypedArray()
        val b = dialog().setTitle(prompt.title)
        when (prompt.type) {
            ChoicePrompt.Type.MULTIPLE -> {
                val checked = BooleanArray(choices.size) { choices[it].selected }
                b.setMultiChoiceItems(labels, checked) { _, i, on -> checked[i] = on }
                    .setPositiveButton(R.string.ok) { _, _ ->
                        answer = prompt.confirm(choices.filterIndexed { i, _ -> checked[i] }.map { it.id }.toTypedArray())
                    }
                    .setNegativeButton(R.string.cancel, null)
            }
            else -> {
                val selected = choices.indexOfFirst { it.selected }
                b.setSingleChoiceItems(labels, selected) { d, i ->
                    if (!choices[i].disabled) {
                        answer = prompt.confirm(choices[i])
                        d.dismiss()
                    }
                }
            }
        }
        b.setOnDismissListener { r.complete(answer ?: prompt.dismiss()) }.show()
        return r
    }

    override fun onBeforeUnloadPrompt(session: GeckoSession, prompt: BeforeUnloadPrompt): GeckoResult<PromptResponse> {
        val r = GeckoResult<PromptResponse>()
        var answer: PromptResponse? = null
        dialog().setTitle(R.string.leave_title).setMessage(R.string.leave_message)
            .setPositiveButton(R.string.leave) { _, _ -> answer = prompt.confirm(AllowOrDeny.ALLOW) }
            .setNegativeButton(R.string.stay) { _, _ -> answer = prompt.confirm(AllowOrDeny.DENY) }
            .setOnDismissListener { r.complete(answer ?: prompt.confirm(AllowOrDeny.DENY)) }
            .show()
        return r
    }

    override fun onRepostConfirmPrompt(session: GeckoSession, prompt: RepostConfirmPrompt): GeckoResult<PromptResponse> {
        val r = GeckoResult<PromptResponse>()
        var answer: PromptResponse? = null
        dialog().setTitle(R.string.resend_title).setMessage(R.string.resend_message)
            .setPositiveButton(R.string.resend) { _, _ -> answer = prompt.confirm(AllowOrDeny.ALLOW) }
            .setNegativeButton(R.string.cancel) { _, _ -> answer = prompt.confirm(AllowOrDeny.DENY) }
            .setOnDismissListener { r.complete(answer ?: prompt.confirm(AllowOrDeny.DENY)) }
            .show()
        return r
    }

    override fun onFilePrompt(session: GeckoSession, prompt: FilePrompt): GeckoResult<PromptResponse> {
        val r = GeckoResult<PromptResponse>()
        pendingFiles?.let { (p, old) -> old.complete(p.dismiss()) }
        val types = prompt.mimeTypes?.filter { it.contains('/') }.orEmpty()
        val intent = Intent(Intent.ACTION_GET_CONTENT)
            .addCategory(Intent.CATEGORY_OPENABLE)
            .setType(if (types.size == 1) types[0] else "*/*")
            .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, prompt.type == FilePrompt.Type.MULTIPLE)
        if (types.size > 1) intent.putExtra(Intent.EXTRA_MIME_TYPES, types.toTypedArray())
        pendingFiles = prompt to r
        try {
            activity.startActivityForResult(intent, MainActivity.PICK_FILES)
        } catch (e: Exception) {
            pendingFiles = null
            r.complete(prompt.dismiss())
        }
        return r
    }

    /** The file picker's answer, from MainActivity.onActivityResult. */
    fun filesPicked(data: Intent?) {
        val (prompt, r) = pendingFiles ?: return
        pendingFiles = null
        val uris = ArrayList<Uri>()
        data?.clipData?.let { clip -> for (i in 0 until clip.itemCount) uris.add(clip.getItemAt(i).uri) }
        if (uris.isEmpty()) data?.data?.let { uris.add(it) }
        r.complete(
            when {
                uris.isEmpty() -> prompt.dismiss()
                prompt.type == FilePrompt.Type.MULTIPLE -> prompt.confirm(activity, uris.toTypedArray())
                else -> prompt.confirm(activity, uris[0])
            },
        )
    }
}
