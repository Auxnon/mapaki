package com.makeavoy.mapaki.data.notes

import java.time.Instant
import java.time.temporal.ChronoUnit
import java.util.Base64

/** An image embedded in a note (or a link's preview), stored as base64 in the file. */
class EmbeddedImage(val name: String, val mime: String, val data: ByteArray) {
    override fun equals(other: Any?) =
        other is EmbeddedImage && other.name == name && other.mime == mime && other.data.contentEquals(data)

    override fun hashCode() = 31 * name.hashCode() + data.contentHashCode()
}

/**
 * A `.note` file: TOML with a title, a body, and images embedded as base64.
 * A body line that is just `![alt](name)` shows that image there; images the
 * body doesn't place show after it. Same format as the desktop client's
 * `note.rs`, byte for byte, so the server can merge edits from both.
 */
data class Note(
    val title: String = "",
    val created: String = "",
    val body: String = "",
    val images: List<EmbeddedImage> = emptyList(),
) {
    /** The explicit title, else the first line of text. */
    val displayTitle: String
        get() = title.trim().ifEmpty {
            body.lineSequence().map { it.trim() }.firstOrNull { it.isNotEmpty() && imageRef(it) == null }?.take(60)
                ?: "(untitled)"
        }

    /** The image a tile shows: the first one the body places, else the first attached. */
    val cover: EmbeddedImage?
        get() = body.lineSequence().mapNotNull(::imageRef)
            .firstNotNullOfOrNull { ref -> images.firstOrNull { it.name == ref } }
            ?: images.firstOrNull()

    /**
     * The body as one short paragraph for tiles and lists: image lines
     * dropped, whitespace collapsed, and cut at a word boundary with "…"
     * past [maxChars]. Without an explicit title the first line is the
     * title, so it is left out here. Same rules as `note.rs`.
     */
    fun snippet(maxChars: Int = SNIPPET_CHARS): String {
        var lines = body.lineSequence().filter { imageRef(it) == null }.map { it.trim() }.filter { it.isNotEmpty() }
        if (title.isBlank()) lines = lines.drop(1)
        val text = lines.joinToString(" ").split(Regex("\\s+")).joinToString(" ")
        if (text.codePointCount(0, text.length) <= maxChars) return text
        var cut = text.offsetByCodePoints(0, maxChars)
        val space = text.lastIndexOf(' ', cut)
        if (space > cut / 2) cut = space
        return text.substring(0, cut).trimEnd() + "…"
    }

    fun toToml(): String = buildString {
        append("title = ").append(basic(title)).append('\n')
        if (created.isNotEmpty()) append("created = ").append(basic(created)).append('\n')
        append("body = ").append(multiline(body)).append('\n')
        for (img in images) {
            append("\n[[images]]\n")
            appendImage(img)
        }
    }

    fun toBytes(): ByteArray = toToml().toByteArray(Charsets.UTF_8)

    companion object {
        const val SNIPPET_CHARS = 160

        fun parse(bytes: ByteArray): Note {
            val text = bytes.toString(Charsets.UTF_8)
            val t = runCatching { Toml.parse(text) }.getOrNull()
            if (t == null || ("body" !in t && "title" !in t)) return Note(body = text)
            return Note(
                title = t.text("title"),
                created = t.text("created"),
                body = t.text("body"),
                images = (t["images"] as? List<*>).orEmpty().mapNotNull { (it as? Map<*, *>)?.image() },
            )
        }
    }
}

/**
 * A `.link` file: the URL plus what it pointed at when saved, and optionally
 * a small cached preview image (the page's og:image, shrunk) so tiles render
 * offline. v0.1 links were just the URL as text; those still open.
 */
data class LinkCard(
    val url: String,
    val title: String = "",
    val description: String = "",
    val site: String = "",
    val saved: String = "",
    val preview: EmbeddedImage? = null,
) {
    val displayTitle: String get() = title.ifBlank { site.ifBlank { url } }

    fun toToml(): String = buildString {
        append("url = ").append(basic(url)).append('\n')
        append("title = ").append(basic(title)).append('\n')
        if (description.isNotEmpty()) append("description = ").append(basic(description)).append('\n')
        if (site.isNotEmpty()) append("site = ").append(basic(site)).append('\n')
        if (saved.isNotEmpty()) append("saved = ").append(basic(saved)).append('\n')
        preview?.let {
            append("\n[preview]\n")
            appendImage(it)
        }
    }

    fun toBytes(): ByteArray = toToml().toByteArray(Charsets.UTF_8)

    companion object {
        fun parse(bytes: ByteArray): LinkCard {
            val text = bytes.toString(Charsets.UTF_8)
            val t = runCatching { Toml.parse(text) }.getOrNull()
            if (t == null || "url" !in t) return LinkCard(url = text.trim())
            return LinkCard(
                url = t.text("url"),
                title = t.text("title"),
                description = t.text("description"),
                site = t.text("site"),
                saved = t.text("saved"),
                preview = (t["preview"] as? Map<*, *>)?.image(),
            )
        }
    }
}

/** `![alt](name)` on a line of its own -> `name`. */
fun imageRef(line: String): String? {
    val l = line.trim()
    if (!l.startsWith("![") || !l.endsWith(")")) return null
    val close = l.indexOf("](")
    if (close < 0) return null
    return l.substring(close + 2, l.length - 1).takeIf { it.isNotEmpty() }
}

fun nowRfc3339(): String = Instant.now().truncatedTo(ChronoUnit.SECONDS).toString()

/** A readable file name for a new note or link, safe on every OS; the server de-duplicates. */
fun fileStemFor(title: String, fallbackPrefix: String): String {
    val stem = title.map { if (it.isLetterOrDigit() || it in " -_()") it else '-' }
        .joinToString("")
        .trim(' ', '-', '.')
        .take(60)
        .trim(' ', '-', '.')
    return if (stem.isEmpty() || stem == "(untitled)") {
        "$fallbackPrefix-${Instant.now().toString().take(10)}"
    } else {
        stem
    }
}

private fun Map<*, *>.text(key: String): String = when (val v = this[key]) {
    null -> ""
    is String -> v
    else -> v.toString()
}

private fun Map<*, *>.image(): EmbeddedImage? {
    val data = text("data").filterNot { it.isWhitespace() }
    val bytes = runCatching { Base64.getDecoder().decode(data) }.getOrNull() ?: return null
    return EmbeddedImage(text("name"), text("type").ifEmpty { "image/jpeg" }, bytes)
}

private fun StringBuilder.appendImage(img: EmbeddedImage) {
    append("name = ").append(basic(img.name)).append('\n')
    append("type = ").append(basic(img.mime)).append('\n')
    append("data = \"").append(Base64.getEncoder().encodeToString(img.data)).append("\"\n")
}

private fun escape(s: String, multiline: Boolean): String = buildString {
    for (c in s) {
        when {
            c == '\\' -> append("\\\\")
            c == '"' && !multiline -> append("\\\"")
            c == '\n' -> append(if (multiline) "\n" else "\\n")
            c == '\t' -> append('\t')
            c == '\r' -> append("\\r")
            c.code < 0x20 || c.code == 0x7f -> append("\\u%04X".format(c.code))
            else -> append(c)
        }
    }
}

private fun basic(s: String) = "\"" + escape(s, multiline = false) + "\""

/** One body line per file line so the server merges line by line; ends in exactly one newline. */
private fun multiline(s: String): String {
    val body = s.trimEnd('\n', '\r')
    if (body.isEmpty()) return "\"\"\"\n\"\"\""
    var esc = escape(body, multiline = true).replace("\"\"\"", "\"\"\\\"")
    if (esc.endsWith('"')) esc = esc.dropLast(1) + "\\\""
    return "\"\"\"\n$esc\n\"\"\""
}
