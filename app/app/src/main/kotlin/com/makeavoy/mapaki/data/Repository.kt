package com.makeavoy.mapaki.data

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Build
import com.makeavoy.mapaki.data.model.FileItem
import com.makeavoy.mapaki.data.net.ApiClient
import com.makeavoy.mapaki.data.notes.LinkCard
import com.makeavoy.mapaki.data.notes.LinkPreview
import com.makeavoy.mapaki.data.notes.Note
import com.makeavoy.mapaki.data.notes.fileStemFor
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

/** A note opened for reading or editing, with the revision its content is. */
data class OpenNote(val item: FileItem, val note: Note, val base: String?)

/**
 * What happened to a share, for the share card: [queued] means it's saved on
 * the phone and goes up on the next sync. [title], [detail] and [image]
 * describe it (a link's page title, site and preview; a note's snippet).
 */
class ShareResult(
    val name: String,
    val queued: Boolean,
    val title: String = name,
    val detail: String = "",
    val image: ByteArray? = null,
)

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
    private val linkPreview = LinkPreview()

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
        val image = bytes.takeIf { mimeType.startsWith("image/") }
        val token = authStore.token
            ?: return ShareResult(name, queued = true, image = image).also { pending.add(name, bytes, mimeType) }
        return try {
            val entry = api.upload(baseUrl(), token, name, bytes, mimeType)
            localIndex.upsertAtTop(entry)
            mirror.write(entry.id, entry.hash.ifEmpty { sha256(bytes) }, bytes)
            ShareResult(entry.name, queued = false, image = image)
        } catch (_: Exception) {
            pending.add(name, bytes, mimeType)
            ShareResult(name, queued = true, image = image)
        }
    }

    /** Saves a new note; queued like any share if the server can't be reached. */
    suspend fun addNote(note: Note): ShareResult {
        val r = addFile(fileStemFor(note.displayTitle, "note") + ".note", note.toBytes(), NOTE_MIME)
        return ShareResult(r.name, r.queued, note.displayTitle, note.snippet(), note.cover?.data)
    }

    /**
     * Saves a shared URL as a `.link`, with the page's title and a small
     * preview image when the page can be fetched (the internet may be up even
     * when the mapaki server isn't, so this happens before queueing).
     */
    suspend fun addLink(url: String, subject: String?): ShareResult {
        val card = linkPreview.card(url, fallbackTitle = subject.orEmpty())
        val r = addFile(fileStemFor(card.displayTitle, "link") + ".link", card.toBytes(), NOTE_MIME)
        return ShareResult(r.name, r.queued, card.displayTitle, card.site.ifEmpty { url }, card.preview?.data)
    }

    /**
     * A note's content and the revision it is. Uses the on-device copy when it
     * matches the latest known head, so notes open offline too.
     */
    suspend fun openNote(id: String): OpenNote? {
        val item = localIndex.items.value.firstOrNull { it.id == id } ?: return null
        val local = mirror.read(id)
        if (local != null && item.hash.isNotEmpty() && mirror.hashOf(id) == item.hash) {
            return OpenNote(item, Note.parse(local), item.head)
        }
        val token = authStore.token
        val fresh = token?.let { runCatching { api.fetchContentWithRev(baseUrl(), it, id) }.getOrNull() }
        if (fresh != null) {
            mirror.write(id, sha256(fresh.bytes), fresh.bytes)
            return OpenNote(item, Note.parse(fresh.bytes), fresh.rev)
        }
        // Stale copy and no server: still readable; editing it would conflict-check against nothing.
        return local?.let { OpenNote(item, Note.parse(it), null) }
    }

    /**
     * Uploads an edited note based on the revision it was opened at. The
     * server fast-forwards or merges cleanly; otherwise this throws
     * [com.makeavoy.mapaki.data.net.ConflictException]. [force] keeps this
     * version over the other device's (which stays in history).
     */
    suspend fun saveNote(open: OpenNote, edited: Note, force: Boolean = false): ApiClient.Saved {
        val token = authStore.token ?: error("not paired")
        val bytes = edited.toBytes()
        val saved = api.putContent(baseUrl(), token, open.item.id, bytes, open.base, force)
        localIndex.updateOne(saved.entry)
        // A merge means the server's copy is ours plus theirs; keep that one.
        val content = if (saved.result == "merged") api.fetchContent(baseUrl(), token, saved.entry.id) else bytes
        mirror.write(saved.entry.id, sha256(content), content)
        return saved
    }

    /** A link's URL, from the on-device copy when there is one. */
    suspend fun linkUrl(id: String): String? = contentBytes(id)?.let { LinkCard.parse(it).url }

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
        private const val NOTE_MIME = "application/toml"

        /** Activity and background worker each build a Repository; never sync twice at once. */
        private val syncLock = Mutex()

        fun sha256(bytes: ByteArray): String =
            MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    }
}
