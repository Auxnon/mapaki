package com.makeavoy.mapaki.data.net

import com.makeavoy.mapaki.data.model.FileItem
import kotlinx.coroutines.Dispatchers
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

class ApiException(message: String, val code: Int = -1) : IOException(message)

class ApiClient(private val client: OkHttpClient) {
    private val json = Json { ignoreUnknownKeys = true }

    @Serializable
    private data class PairRequest(val otp: String)

    @Serializable
    private data class PairResponse(val token: String)

    @Serializable
    private data class RenameRequest(val name: String)

    @Serializable
    private data class OrderRequest(val ids: List<String>)

    suspend fun pair(baseUrl: String, otp: String): String = withContext(Dispatchers.IO) {
        val body = json.encodeToString(PairRequest(otp)).toRequestBody(JSON)
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

    suspend fun delete(baseUrl: String, token: String, id: String) = withContext(Dispatchers.IO) {
        val request = Request.Builder()
            .url(baseUrl.trimEnd('/') + "/files/$id")
            .delete()
            .build()
        execute(request, token)
        Unit
    }

    suspend fun setOrder(baseUrl: String, token: String, ids: List<String>): List<FileItem> =
        withContext(Dispatchers.IO) {
            val body = json.encodeToString(OrderRequest(ids)).toRequestBody(JSON)
            val request = Request.Builder().url(baseUrl.trimEnd('/') + "/order").put(body).build()
            json.decodeFromString(execute(request, token))
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
        return ApiException("HTTP $code from ${request.url}: $detail", code)
    }

    companion object {
        private val JSON = "application/json".toMediaTypeOrNull()
    }
}
