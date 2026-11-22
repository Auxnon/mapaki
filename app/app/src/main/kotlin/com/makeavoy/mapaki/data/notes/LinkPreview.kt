package com.makeavoy.mapaki.data.notes

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import java.util.concurrent.TimeUnit

/**
 * Builds a [LinkCard] for a shared URL: the page's title, description and a
 * small preview of its og:image. Best effort and quick: anything that fails
 * or takes too long just leaves those fields empty.
 */
class LinkPreview(base: OkHttpClient = OkHttpClient()) {
    private val client = base.newBuilder()
        .connectTimeout(4, TimeUnit.SECONDS)
        .readTimeout(4, TimeUnit.SECONDS)
        .callTimeout(8, TimeUnit.SECONDS)
        .build()

    suspend fun card(url: String, fallbackTitle: String = ""): LinkCard = withContext(Dispatchers.IO) {
        val parsed = url.toHttpUrlOrNull()
        val bare = LinkCard(url = url, title = fallbackTitle, site = parsed?.host?.removePrefix("www.").orEmpty(), saved = nowRfc3339())
        if (parsed == null) return@withContext bare
        runCatching {
            val (contentType, body) = get(url, MAX_PAGE_BYTES) ?: return@runCatching bare
            if (contentType.startsWith("image/")) {
                return@runCatching bare.copy(preview = Images.preview(body)?.let { EmbeddedImage("preview.jpg", "image/jpeg", it) })
            }
            val meta = parseMeta(body.toString(Charsets.UTF_8))
            val imageUrl = (meta["og:image"] ?: meta["twitter:image"])?.let { parsed.resolve(it)?.toString() }
            val preview = imageUrl
                ?.let { get(it, MAX_IMAGE_BYTES)?.second }
                ?.let { Images.preview(it) }
                ?.let { EmbeddedImage("preview.jpg", "image/jpeg", it) }
            bare.copy(
                title = (meta["og:title"] ?: meta["twitter:title"] ?: meta["title"])?.take(200) ?: fallbackTitle,
                description = (meta["og:description"] ?: meta["description"])?.take(300).orEmpty(),
                site = meta["og:site_name"]?.take(80) ?: bare.site,
                preview = preview,
            )
        }.getOrDefault(bare)
    }

    private fun get(url: String, limit: Long): Pair<String, ByteArray>? {
        val request = Request.Builder().url(url)
            .header("User-Agent", "Mozilla/5.0 (Android) mapaki link preview")
            .header("Accept", "text/html,image/*;q=0.8,*/*;q=0.5")
            .build()
        client.newCall(request).execute().use { response ->
            if (!response.isSuccessful) return null
            val body = response.body ?: return null
            val source = body.source()
            source.request(limit)
            val bytes = source.buffer.readByteArray(minOf(source.buffer.size, limit))
            return (response.header("Content-Type") ?: "").lowercase() to bytes
        }
    }

    companion object {
        private const val MAX_PAGE_BYTES = 512L * 1024
        private const val MAX_IMAGE_BYTES = 5L * 1024 * 1024

        private val metaTag = Regex("<meta\\s[^>]*>", RegexOption.IGNORE_CASE)
        private val attr = Regex("([a-zA-Z:_-]+)\\s*=\\s*(\"[^\"]*\"|'[^']*'|[^\\s>]+)")
        private val titleTag = Regex("<title[^>]*>([^<]*)</title>", RegexOption.IGNORE_CASE)

        /** og:/twitter:/description meta tags and `<title>`, entity-decoded. First one wins. */
        fun parseMeta(html: String): Map<String, String> {
            val out = LinkedHashMap<String, String>()
            val head = html.take(256 * 1024)
            for (tag in metaTag.findAll(head)) {
                val attrs = attr.findAll(tag.value).associate { m ->
                    m.groupValues[1].lowercase() to m.groupValues[2].trim('"', '\'')
                }
                val key = (attrs["property"] ?: attrs["name"])?.lowercase() ?: continue
                val content = attrs["content"]?.let(::decodeEntities)?.trim()?.takeIf { it.isNotEmpty() } ?: continue
                out.putIfAbsent(key, content)
            }
            titleTag.find(head)?.groupValues?.get(1)?.let(::decodeEntities)?.trim()?.takeIf { it.isNotEmpty() }
                ?.let { out.putIfAbsent("title", it) }
            return out
        }

        private val entity = Regex("&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z]+);")
        private val named = mapOf("amp" to "&", "lt" to "<", "gt" to ">", "quot" to "\"", "apos" to "'", "nbsp" to " ")

        private fun codePoint(cp: Int): String? = runCatching { String(Character.toChars(cp)) }.getOrNull()

        fun decodeEntities(s: String): String = entity.replace(s) { m ->
            val e = m.groupValues[1]
            when {
                e.startsWith("#x") -> e.drop(2).toIntOrNull(16)?.let(::codePoint)
                e.startsWith("#") -> e.drop(1).toIntOrNull()?.let(::codePoint)
                else -> named[e]
            } ?: m.value
        }
    }
}
