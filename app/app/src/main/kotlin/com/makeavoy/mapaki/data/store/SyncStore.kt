package com.makeavoy.mapaki.data.store

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import java.io.File
import java.util.UUID

/** Change-feed cursor and the background check interval (see SyncScheduler). */
class SyncPrefs(context: Context) {
    private val prefs = context.getSharedPreferences("mapaki_sync", Context.MODE_PRIVATE)

    var seq: Long
        get() = prefs.getLong(KEY_SEQ, -1)
        set(value) = prefs.edit().putLong(KEY_SEQ, value).apply()

    var backgroundDelayMinutes: Long
        get() = prefs.getLong(KEY_DELAY, 0)
        set(value) = prefs.edit().putLong(KEY_DELAY, value).apply()

    companion object {
        private const val KEY_SEQ = "seq"
        private const val KEY_DELAY = "background_delay_minutes"
    }
}

/**
 * Every file's bytes kept on device, keyed by id, plus the content hash each
 * copy has so a sync only downloads what actually changed.
 */
class MirrorStore(context: Context) {
    private val dir = File(context.filesDir, "mirror").apply { mkdirs() }
    private val indexFile = File(dir, ".hashes.json")
    private val json = Json { ignoreUnknownKeys = true }
    private val hashes: MutableMap<String, String> = try {
        json.decodeFromString<Map<String, String>>(indexFile.readText()).toMutableMap()
    } catch (_: Exception) {
        mutableMapOf()
    }

    fun hashOf(id: String): String? = hashes[id]?.takeIf { File(dir, id).exists() }

    fun read(id: String): ByteArray? = file(id)?.readBytes()

    /** The on-device copy itself, for streaming instead of reading it whole. */
    fun file(id: String): File? = File(dir, id).takeIf { it.exists() }

    @Synchronized
    fun write(id: String, hash: String, bytes: ByteArray) {
        val tmp = File(dir, "$id.tmp")
        tmp.writeBytes(bytes)
        tmp.renameTo(File(dir, id))
        hashes[id] = hash
        persist()
    }

    /** Drops copies of files that no longer exist on the server. */
    @Synchronized
    fun retainOnly(ids: Set<String>) {
        dir.listFiles()?.forEach { f ->
            if (!f.name.startsWith(".") && f.name !in ids) f.delete()
        }
        hashes.keys.retainAll(ids)
        persist()
    }

    private fun persist() {
        try {
            indexFile.writeText(json.encodeToString(hashes.toMap()))
        } catch (_: Exception) {
        }
    }
}

/** Shares that couldn't reach the server yet; flushed at the start of every sync. */
class PendingUploads(context: Context) {
    @Serializable
    data class Meta(val name: String, val mimeType: String, val createdAt: Long)

    data class Pending(val key: String, val meta: Meta, val bytes: ByteArray)

    private val dir = File(context.filesDir, "pending").apply { mkdirs() }
    private val json = Json { ignoreUnknownKeys = true }

    fun add(name: String, bytes: ByteArray, mimeType: String) {
        val key = "${System.currentTimeMillis()}-${UUID.randomUUID()}"
        File(dir, "$key.bin").writeBytes(bytes)
        File(dir, "$key.json").writeText(json.encodeToString(Meta(name, mimeType, System.currentTimeMillis())))
    }

    fun list(): List<Pending> =
        (dir.listFiles { f -> f.name.endsWith(".json") } ?: emptyArray())
            .sortedBy { it.name }
            .mapNotNull { metaFile ->
                val key = metaFile.name.removeSuffix(".json")
                val bin = File(dir, "$key.bin")
                runCatching { Pending(key, json.decodeFromString(metaFile.readText()), bin.readBytes()) }.getOrNull()
            }

    fun remove(key: String) {
        File(dir, "$key.json").delete()
        File(dir, "$key.bin").delete()
    }

    val count: Int get() = dir.listFiles { f -> f.name.endsWith(".json") }?.size ?: 0
}
