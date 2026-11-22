package com.makeavoy.mapaki.data

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Build
import com.makeavoy.mapaki.data.model.FileItem
import com.makeavoy.mapaki.data.net.ApiClient
import com.makeavoy.mapaki.data.net.MdnsDns
import com.makeavoy.mapaki.data.net.MdnsResolver
import com.makeavoy.mapaki.data.net.currentBaseUrl
import com.makeavoy.mapaki.data.net.isOnHomeNetwork
import com.makeavoy.mapaki.data.net.ssidReadIssue
import com.makeavoy.mapaki.data.store.AuthStore
import com.makeavoy.mapaki.data.store.LocalIndexStore
import com.makeavoy.mapaki.data.store.MirrorStore
import com.makeavoy.mapaki.data.store.PendingUploads
import com.makeavoy.mapaki.data.store.SyncPrefs
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import okhttp3.OkHttpClient
import java.security.MessageDigest
import java.util.concurrent.TimeUnit

/** What happened to a share, for the toast. */
sealed interface ShareResult {
    data class Synced(val name: String) : ShareResult
    data class Queued(val name: String) : ShareResult
}

class Repository(private val context: Context) {
    val authStore = AuthStore(context)
    private val localIndex = LocalIndexStore(context)
    private val mirror = MirrorStore(context)
    private val pending = PendingUploads(context)
    private val prefs = SyncPrefs(context)
    val items: StateFlow<List<FileItem>> = localIndex.items

    private val wifiManager =
        context.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
    private val httpClient = OkHttpClient.Builder()
        .dns(MdnsDns(MdnsResolver(wifiManager)))
        .connectTimeout(5, TimeUnit.SECONDS)
        .readTimeout(60, TimeUnit.SECONDS)
        .build()
    private val api = ApiClient(httpClient)

    val isPaired: Boolean get() = authStore.token != null

    val pendingCount: Int get() = pending.count

    /** What the pairing screen shows so a wrong-network mismatch is obvious instead of a mystery failure. */
    fun targetDescription(): String {
        val onHome = isOnHomeNetwork(context)
        val status = if (onHome) "on home network" else "not on home network"
        val issue = ssidReadIssue(context)?.let { " — $it" }.orEmpty()
        return "${baseUrl()} ($status)$issue"
    }

    private fun baseUrl() = currentBaseUrl(context)

    suspend fun pair(otp: String) {
        // The daemon's OTP alphabet (data_encoding::BASE32_NOPAD) is uppercase-only and
        // compared byte-for-byte, so a lowercased keyboard autocorrect looks right to the
        // eye but silently fails — normalize case here rather than relying on the keyboard.
        val device = "${Build.MANUFACTURER} ${Build.MODEL}".trim()
        val token = api.pair(baseUrl(), otp.trim().uppercase(), device)
        authStore.token = token
        prefs.seq = -1
        sync()
    }

    /**
     * One fresh check against the server: flush queued shares, ask the change
     * feed whether anything moved since our cursor, and only then refetch the
     * list and download changed content. Returns true if anything changed.
     */
    suspend fun sync(): Boolean = syncLock.withLock {
        val token = authStore.token ?: return false
        val base = baseUrl()
        var changed = flushPending(base, token)

        val changes = api.changes(base, token, prefs.seq.coerceAtLeast(0))
        if (changes.seq != prefs.seq || changes.entries.isNotEmpty() || changed) {
            localIndex.replaceAll(api.listFiles(base, token))
            prefs.seq = changes.seq
            changed = true
        }
        mirrorAll(base, token)
        changed
    }

    private suspend fun flushPending(base: String, token: String): Boolean {
        var any = false
        for (p in pending.list()) {
            val entry = api.upload(base, token, p.meta.name, p.bytes, p.meta.mimeType)
            mirror.write(entry.id, entry.hash.ifEmpty { sha256(p.bytes) }, p.bytes)
            pending.remove(p.key)
            any = true
        }
        return any
    }

    /** Keeps a full local copy of every file; only changed hashes are fetched. */
    private suspend fun mirrorAll(base: String, token: String) {
        val current = localIndex.items.value
        for (item in current) {
            if (item.hash.isNotEmpty() && mirror.hashOf(item.id) == item.hash) continue
            try {
                val bytes = api.fetchContent(base, token, item.id)
                mirror.write(item.id, item.hash.ifEmpty { sha256(bytes) }, bytes)
            } catch (_: Exception) {
                // Leave it for the next pass; one bad file shouldn't stop the rest.
            }
        }
        mirror.retainOnly(current.map { it.id }.toSet())
    }

    /** Uploads a shared file, or queues it on device if the server isn't reachable. */
    suspend fun addFile(name: String, bytes: ByteArray, mimeType: String): ShareResult {
        val token = authStore.token ?: return ShareResult.Queued(name).also { pending.add(name, bytes, mimeType) }
        return try {
            val entry = api.upload(baseUrl(), token, name, bytes, mimeType)
            localIndex.upsertAtTop(entry)
            mirror.write(entry.id, entry.hash.ifEmpty { sha256(bytes) }, bytes)
            ShareResult.Synced(entry.name)
        } catch (_: Exception) {
            pending.add(name, bytes, mimeType)
            ShareResult.Queued(name)
        }
    }

    suspend fun rename(id: String, newName: String) {
        val token = authStore.token ?: return
        localIndex.updateOne(api.rename(baseUrl(), token, id, newName))
    }

    /**
     * Deletes the revision we last saw. Throws [com.makeavoy.mapaki.data.net.ConflictException]
     * if another device changed the file since; pass [force] to delete anyway.
     */
    suspend fun delete(id: String, force: Boolean = false) {
        val token = authStore.token ?: return
        val base = if (force) null else localIndex.items.value.firstOrNull { it.id == id }?.head
        api.delete(baseUrl(), token, id, base)
        localIndex.remove(id)
        mirror.retainOnly(localIndex.items.value.map { it.id }.toSet())
    }

    suspend fun reorder(orderedIds: List<String>) {
        val token = authStore.token ?: return
        localIndex.replaceAll(api.setOrder(baseUrl(), token, orderedIds))
    }

    /** Bytes for previewing/exporting an item, from the on-device mirror first. */
    suspend fun contentBytes(id: String): ByteArray? {
        mirror.read(id)?.let { return it }
        val token = authStore.token ?: return null
        return try {
            val bytes = api.fetchContent(baseUrl(), token, id)
            val hash = localIndex.items.value.firstOrNull { it.id == id }?.hash?.ifEmpty { null } ?: sha256(bytes)
            mirror.write(id, hash, bytes)
            bytes
        } catch (_: Exception) {
            null
        }
    }

    /** Suspends while the server's event stream is open, syncing on every change. */
    suspend fun listenForChanges(onChanged: suspend () -> Unit) {
        val token = authStore.token ?: return
        api.listen(baseUrl(), token) { seq ->
            if (seq != prefs.seq) onChanged()
        }
    }

    companion object {
        /** Activity and background worker each build a Repository; never sync twice at once. */
        private val syncLock = Mutex()

        fun sha256(bytes: ByteArray): String =
            MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    }
}
