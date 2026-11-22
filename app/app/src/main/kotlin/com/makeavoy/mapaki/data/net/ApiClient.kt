package com.makeavoy.mapaki.data.net

import com.makeavoy.mapaki.data.model.FileItem
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.Job
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.Serializable
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.MultipartBody
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import java.io.IOException
import java.util.concurrent.TimeUnit

class ApiException(message: String, val code: Int = -1, val body: String = "") : IOException(message)

/** 409 from the server: the file changed since the revision we based an action on. */
class ConflictException(val current: FileItem) : IOException("conflict on ${current.name}")

@Serializable
data class Changes(val seq: Long, val entries: List<FileItem>)

class ApiClient(private val client: OkHttpClient) {
    private val json = Json { ignoreUnknownKeys = true; coerceInputValues = true }

    @Serializable
    private data class PairRequest(val otp: String, val device: String)

    @Serializable
    private data class PairResponse(val token: String)

    @Serializable
    private data class RenameRequest(val name: String)

    @Serializable
    private data class OrderRequest(val ids: List<String>)

    @Serializable
    private data class ConflictBody(val entry: FileItem)

    @Serializable
    private data class PutResponse(val result: String, val entry: FileItem)

    /** Bytes plus the revision they are (`x-mapaki-rev`), to base an edit on. */
    class Content(val bytes: ByteArray, val rev: String?)

    /** Outcome of a content upload: "fast_forward", "merged" or "unchanged", and the new entry. */
    class Saved(val result: String, val entry: FileItem)

    suspend fun pair(baseUrl: String, otp: String, device: String): String = withContext(Dispatchers.IO) {
        val body = json.encodeToString(PairRequest(otp, device)).toRequestBody(JSON)
        val request = Request.Builder().url(baseUrl.trimEnd('/') + "/pair").post(body).build()
        val text = execute(request, token = null)
        json.decodeFromString<PairResponse>(text).token
    }

    suspend fun listFiles(baseUrl: String, token: String): List<FileItem> =
        withContext(Dispatchers.IO) {
            val request = Request.Builder().url(baseUrl.trimEnd('/') + "/files").get().build()
            json.decodeFromString(execute(request, token))
        }

    suspend fun upload(
        baseUrl: String,
        token: String,
        name: String,
        bytes: ByteArray,
        mimeType: String,
    ): FileItem = withContext(Dispatchers.IO) {
        val body = MultipartBody.Builder()
            .setType(MultipartBody.FORM)
            .addFormDataPart("name", name)
            .addFormDataPart(
                "file",
                name,
                bytes.toRequestBody(mimeType.toMediaTypeOrNull()),
            )
            .build()
        val request = Request.Builder().url(baseUrl.trimEnd('/') + "/files").post(body).build()
        json.decodeFromString(execute(request, token))
    }

    suspend fun rename(baseUrl: String, token: String, id: String, newName: String): FileItem =
        withContext(Dispatchers.IO) {
            val body = json.encodeToString(RenameRequest(newName)).toRequestBody(JSON)
            val request = Request.Builder()
                .url(baseUrl.trimEnd('/') + "/files/$id")
                .patch(body)
                .build()
            json.decodeFromString(execute(request, token))
        }

    /** With a [base] revision the server refuses (409) if the file changed since. */
    suspend fun delete(baseUrl: String, token: String, id: String, base: String?) = withContext(Dispatchers.IO) {
        val query = if (base.isNullOrEmpty()) "" else "?base=$base"
        val request = Request.Builder()
            .url(baseUrl.trimEnd('/') + "/files/$id$query")
            .delete()
            .build()
        try {
            execute(request, token)
        } catch (e: ApiException) {
            if (e.code != 409) throw e
            throw ConflictException(json.decodeFromString<ConflictBody>(e.body).entry)
        }
        Unit
    }

    suspend fun changes(baseUrl: String, token: String, since: Long): Changes =
        withContext(Dispatchers.IO) {
            val request = Request.Builder().url(baseUrl.trimEnd('/') + "/changes?since=$since").get().build()
            json.decodeFromString(execute(request, token))
        }

    /**
     * Holds the server's event stream open, calling [onSeq] per change, until
     * the connection drops or the calling coroutine is cancelled. The read
     * timeout outlasts the server's 75 s keep-alive so a dead link is noticed.
     */
    suspend fun listen(baseUrl: String, token: String, onSeq: suspend (Long) -> Unit) {
        val streamClient = client.newBuilder().readTimeout(200, TimeUnit.SECONDS).build()
        val request = withAuth(
            Request.Builder().url(baseUrl.trimEnd('/') + "/events").header("Accept", "text/event-stream").build(),
            token,
        )
        val call = streamClient.newCall(request)
        val job = currentCoroutineContext()[Job]
        val handle = job?.invokeOnCompletion { call.cancel() }
        try {
            withContext(Dispatchers.IO) {
                call.execute().use { response ->
                    if (!response.isSuccessful) throw httpError(request, response.code, response.body?.string())
                    val source = response.body?.source() ?: return@use
                    while (true) {
                        val line = source.readUtf8Line() ?: break
                        if (line.startsWith("data:")) {
                            line.removePrefix("data:").trim().toLongOrNull()?.let { onSeq(it) }
                        }
                    }
                }
            }
        } finally {
            handle?.dispose()
        }
    }

    suspend fun setOrder(baseUrl: String, token: String, ids: List<String>): List<FileItem> =
        withContext(Dispatchers.IO) {
            val body = json.encodeToString(OrderRequest(ids)).toRequestBody(JSON)
            val request = Request.Builder().url(baseUrl.trimEnd('/') + "/order").put(body).build()
            json.decodeFromString(execute(request, token))
        }

    /**
     * New content for an existing file, based on revision [base]. The server
     * fast-forwards, merges text edits cleanly, or throws [ConflictException];
     * [force] overwrites (still recorded as a new revision).
     */
    suspend fun putContent(
        baseUrl: String,
        token: String,
        id: String,
        bytes: ByteArray,
        base: String?,
        force: Boolean,
    ): Saved = withContext(Dispatchers.IO) {
        val query = buildString {
            append("?force=").append(force)
            if (!base.isNullOrEmpty()) append("&base=").append(base)
        }
        val request = Request.Builder()
            .url(baseUrl.trimEnd('/') + "/files/$id/content$query")
            .put(bytes.toRequestBody(OCTETS))
            .build()
        val text = try {
            execute(request, token)
        } catch (e: ApiException) {
            if (e.code != 409) throw e
            throw ConflictException(json.decodeFromString<ConflictBody>(e.body).entry)
        }
        json.decodeFromString<PutResponse>(text).let { Saved(it.result, it.entry) }
    }

    suspend fun fetchContentWithRev(baseUrl: String, token: String, id: String): Content =
        withContext(Dispatchers.IO) {
            val request = Request.Builder().url(baseUrl.trimEnd('/') + "/files/$id/content").get().build()
            client.newCall(withAuth(request, token)).execute().use { response ->
                if (!response.isSuccessful) throw httpError(request, response.code, response.body?.string())
                Content(response.body?.bytes() ?: ByteArray(0), response.header("x-mapaki-rev"))
            }
        }

    suspend fun fetchContent(baseUrl: String, token: String, id: String): ByteArray =
        withContext(Dispatchers.IO) {
            val request = Request.Builder()
                .url(baseUrl.trimEnd('/') + "/files/$id/content")
                .get()
                .build()
            client.newCall(withAuth(request, token)).execute().use { response ->
                if (!response.isSuccessful) {
                    throw httpError(request, response.code, response.body?.string())
                }
                response.body?.bytes() ?: ByteArray(0)
            }
        }

    private fun withAuth(request: Request, token: String?): Request =
        if (token == null) request else request.newBuilder().addHeader("Authorization", "Bearer $token").build()

    /** Always names the URL actually hit and the raw status/body — a blank body is not swallowed into "request failed". */
    private fun execute(request: Request, token: String?): String {
        val response = try {
            client.newCall(withAuth(request, token)).execute()
        } catch (e: IOException) {
            throw ApiException("couldn't reach ${request.url}: ${e.javaClass.simpleName} ${e.message}", -1)
        }
        response.use {
            val text = it.body?.string().orEmpty()
            if (!it.isSuccessful) {
                throw httpError(request, it.code, text)
            }
            return text
        }
    }

    private fun httpError(request: Request, code: Int, body: String?): ApiException {
        val detail = body?.takeIf { it.isNotBlank() } ?: "(empty response body)"
        return ApiException("HTTP $code from ${request.url}: $detail", code, body.orEmpty())
    }

    companion object {
        private val JSON = "application/json".toMediaTypeOrNull()
        private val OCTETS = "application/octet-stream".toMediaTypeOrNull()
    }
}
