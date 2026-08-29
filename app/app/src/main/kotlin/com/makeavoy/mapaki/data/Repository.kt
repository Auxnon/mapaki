package com.makeavoy.mapaki.data

import android.content.Context
import android.net.wifi.WifiManager
import com.makeavoy.mapaki.data.model.FileItem
import com.makeavoy.mapaki.data.net.ApiClient
import com.makeavoy.mapaki.data.net.MdnsDns
import com.makeavoy.mapaki.data.net.MdnsResolver
import com.makeavoy.mapaki.data.net.currentBaseUrl
import com.makeavoy.mapaki.data.store.AuthStore
import com.makeavoy.mapaki.data.store.LocalIndexStore
import kotlinx.coroutines.flow.StateFlow
import okhttp3.OkHttpClient
import java.io.File
import java.util.concurrent.TimeUnit

class Repository(private val context: Context) {
    val authStore = AuthStore(context)
    private val localIndex = LocalIndexStore(context)
    val items: StateFlow<List<FileItem>> = localIndex.items

    private val cacheDir = File(context.filesDir, "cache").apply { mkdirs() }

    private val wifiManager =
        context.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
    private val httpClient = OkHttpClient.Builder()
        .dns(MdnsDns(MdnsResolver(wifiManager)))
        .connectTimeout(5, TimeUnit.SECONDS)
        .readTimeout(20, TimeUnit.SECONDS)
        .build()
    private val api = ApiClient(httpClient)

    val isPaired: Boolean get() = authStore.token != null

    private fun baseUrl() = currentBaseUrl(context)

    suspend fun pair(otp: String) {
        val token = api.pair(baseUrl(), otp.trim())
        authStore.token = token
        sync()
    }

    suspend fun sync() {
        val token = authStore.token ?: return
        localIndex.replaceAll(api.listFiles(baseUrl(), token))
    }

    suspend fun addFile(name: String, bytes: ByteArray, mimeType: String) {
        val token = authStore.token ?: return
        val entry = api.upload(baseUrl(), token, name, bytes, mimeType)
        localIndex.upsertAtTop(entry)
        cacheBytes(entry.id, bytes)
    }

    suspend fun rename(id: String, newName: String) {
        val token = authStore.token ?: return
        localIndex.updateOne(api.rename(baseUrl(), token, id, newName))
    }

    suspend fun delete(id: String) {
        val token = authStore.token ?: return
        api.delete(baseUrl(), token, id)
        localIndex.remove(id)
        File(cacheDir, id).delete()
    }

    suspend fun reorder(orderedIds: List<String>) {
        val token = authStore.token ?: return
        localIndex.replaceAll(api.setOrder(baseUrl(), token, orderedIds))
    }

    /** Bytes for previewing/exporting an item; served from a small on-disk cache first. */
    suspend fun contentBytes(id: String): ByteArray? {
        val cached = File(cacheDir, id)
        if (cached.exists()) return cached.readBytes()
        val token = authStore.token ?: return null
        return try {
            val bytes = api.fetchContent(baseUrl(), token, id)
            cacheBytes(id, bytes)
            bytes
        } catch (_: Exception) {
            null
        }
    }

    private fun cacheBytes(id: String, bytes: ByteArray) {
        try {
            trimCacheIfNeeded()
            File(cacheDir, id).writeBytes(bytes)
        } catch (_: Exception) {
        }
    }

    private fun trimCacheIfNeeded() {
        val files = cacheDir.listFiles() ?: return
        if (files.size < MAX_CACHED_FILES) return
        files.sortedBy { it.lastModified() }
            .take(files.size - MAX_CACHED_FILES + 1)
            .forEach { it.delete() }
    }

    companion object {
        private const val MAX_CACHED_FILES = 100
    }
}
