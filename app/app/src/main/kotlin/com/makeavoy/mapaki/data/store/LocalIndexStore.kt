package com.makeavoy.mapaki.data.store

import android.content.Context
import com.makeavoy.mapaki.data.model.FileItem
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import java.io.File

/**
 * Persists the ordered file list (metadata only, not bytes) as JSON so the UI
 * can render instantly on launch from the last-known-good state, before any
 * network round trip completes. The list's array order *is* the display
 * order — server `position` is informational only and never re-sorted on
 * device, so a locally-reordered list is never fought by stale positions.
 */
class LocalIndexStore(context: Context) {
    private val file = File(context.filesDir, "files-index.json")
    private val json = Json { ignoreUnknownKeys = true; coerceInputValues = true }

    private val _items = MutableStateFlow(load())
    val items: StateFlow<List<FileItem>> = _items

    private fun load(): List<FileItem> = try {
        if (file.exists()) json.decodeFromString(file.readText()) else emptyList()
    } catch (_: Exception) {
        emptyList()
    }

    fun replaceAll(newItems: List<FileItem>) {
        _items.value = newItems
        persist()
    }

    fun upsertAtTop(item: FileItem) {
        _items.value = listOf(item) + _items.value.filter { it.id != item.id }
        persist()
    }

    fun updateOne(item: FileItem) {
        _items.value = _items.value.map { if (it.id == item.id) item else it }
        persist()
    }

    fun remove(id: String) {
        _items.value = _items.value.filter { it.id != id }
        persist()
    }

    private fun persist() {
        try {
            file.writeText(json.encodeToString(_items.value))
        } catch (_: Exception) {
        }
    }
}
