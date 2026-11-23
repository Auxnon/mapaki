package com.makeavoy.mapaki.data

import android.content.Context
import android.content.Intent
import android.net.Uri
import com.makeavoy.mapaki.data.net.displayNameFor
import com.makeavoy.mapaki.data.net.mimeTypeFor
import com.makeavoy.mapaki.data.net.readAllBytes
import com.makeavoy.mapaki.data.notes.Note
import com.makeavoy.mapaki.data.notes.nowRfc3339

/** Turns a share intent into files in the folder. */
object Sharing {
    private val URL = Regex("https?://[^\\s<>\"]+")

    /** One shared thing read off the intent, before it's saved. */
    sealed interface Item {
        data class Text(val text: String, val subject: String?) : Item
        data class Stream(val uri: Uri) : Item
    }

    @Suppress("DEPRECATION")
    fun itemsOf(intent: Intent): List<Item> = when (intent.action) {
        Intent.ACTION_SEND -> {
            val uri = intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM)
            val text = intent.getStringExtra(Intent.EXTRA_TEXT)
            when {
                uri != null -> listOf(Item.Stream(uri))
                text != null -> listOf(Item.Text(text, intent.getStringExtra(Intent.EXTRA_SUBJECT)))
                else -> emptyList()
            }
        }
        Intent.ACTION_SEND_MULTIPLE ->
            intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM).orEmpty().map { Item.Stream(it) }
        else -> emptyList()
    }

    /** Saves one item; null if a shared file couldn't be read. */
    suspend fun save(context: Context, repo: Repository, item: Item): ShareResult? = when (item) {
        is Item.Text -> saveText(repo, item.text, item.subject)
        is Item.Stream -> {
            val bytes = readAllBytes(context.contentResolver, item.uri)
            bytes?.let {
                val name = displayNameFor(context, item.uri, fallback = "file-${System.currentTimeMillis()}")
                repo.addFile(name, it, mimeTypeFor(context, item.uri))
            }
        }
    }

    /**
     * Shared text: a URL (optionally with a few words around it, as many apps
     * send "Page title https://...") becomes a `.link`, anything else a `.note`.
     */
    suspend fun saveText(repo: Repository, text: String, subject: String?): ShareResult {
        val url = URL.find(text)?.value
        val rest = url?.let { text.replace(it, "").trim() }
        return if (url != null && rest!!.length <= 200 && '\n' !in rest) {
            repo.addLink(url, subject?.takeIf { it.isNotBlank() } ?: rest.ifEmpty { null })
        } else {
            repo.addNote(Note(title = subject.orEmpty().trim(), created = nowRfc3339(), body = text))
        }
    }
}
