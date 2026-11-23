package com.makeavoy.mapaki.ui

import android.app.Application
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.makeavoy.mapaki.data.OpenNote
import com.makeavoy.mapaki.data.Repository
import com.makeavoy.mapaki.data.ShareResult
import com.makeavoy.mapaki.data.model.FileItem
import com.makeavoy.mapaki.data.model.isLink
import com.makeavoy.mapaki.data.model.isNote
import com.makeavoy.mapaki.data.net.ConflictException
import com.makeavoy.mapaki.data.notes.Note
import com.makeavoy.mapaki.data.notes.nowRfc3339
import com.makeavoy.mapaki.sync.SyncScheduler
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/** A delete that raced an edit on another device, waiting on the user. */
data class DeleteConflict(val item: FileItem)

/** A full-screen page over the grid. */
sealed interface Page {
    data class Viewing(val open: OpenNote) : Page

    /** [open] is null for a new note. */
    data class Editing(val open: OpenNote?, val draft: Note) : Page
}

/** An edited note whose original changed elsewhere in a way that didn't merge. */
data class NoteConflict(val open: OpenNote, val mine: Note)

enum class Keep { MINE, THEIRS, BOTH }

class MapakiViewModel(app: Application) : AndroidViewModel(app) {
    private val repo = Repository(app)

    val items: StateFlow<List<FileItem>> = repo.items

    var isPaired by mutableStateOf(repo.isPaired)
        private set
    var isBusy by mutableStateOf(false)
        private set
    var pairError by mutableStateOf<String?>(null)
        private set
    var deleteConflict by mutableStateOf<DeleteConflict?>(null)
        private set

    var page by mutableStateOf<Page?>(null)
        private set
    var isSaving by mutableStateOf(false)
        private set
    var noteConflict by mutableStateOf<NoteConflict?>(null)
        private set

    private val _openFiles = MutableSharedFlow<java.io.File>(extraBufferCapacity = 1)
    /** Files to hand to another app to open (MainActivity starts the viewer). */
    val openFiles: SharedFlow<java.io.File> = _openFiles

    private val _openUrls = MutableSharedFlow<String>(extraBufferCapacity = 1)
    /** Links the user tapped, for the activity to hand to a browser. */
    val openUrls: SharedFlow<String> = _openUrls

    private val _toasts = MutableSharedFlow<String>(extraBufferCapacity = 8)
    /** Short messages for a Toast: share results, sync failures worth knowing about. */
    val toasts: SharedFlow<String> = _toasts

    val targetDescription: String get() = repo.targetDescription()

    private var liveJob: Job? = null

    fun pair(otp: String) {
        viewModelScope.launch {
            isBusy = true
            pairError = null
            try {
                repo.pair(otp)
                isPaired = true
                onForeground()
            } catch (e: Exception) {
                pairError = e.message ?: "pairing failed"
            } finally {
                isBusy = false
            }
        }
    }

    /**
     * App came on screen: fresh sync, then hold the server's event stream
     * open so edits from other devices show up live. Reconnects with the same
     * ×4 backoff as the desktop client while the server is unreachable.
     */
    fun onForeground() {
        if (!repo.isPaired) return
        SyncScheduler.reset(getApplication())
        refresh()
        if (liveJob?.isActive == true) return
        liveJob = viewModelScope.launch {
            var retryMs = MIN_RETRY_MS
            while (isActive) {
                val started = System.currentTimeMillis()
                try {
                    repo.listenForChanges { refreshNow() }
                } catch (_: Exception) {
                }
                retryMs = if (System.currentTimeMillis() - started > MIN_RETRY_MS) MIN_RETRY_MS
                else (retryMs * 4).coerceAtMost(MAX_RETRY_MS)
                delay(retryMs)
            }
        }
    }

    /** Off screen: drop the live connection; the background worker takes over. */
    fun onBackground() {
        liveJob?.cancel()
        liveJob = null
    }

    fun refresh() {
        if (!repo.isPaired) return
        viewModelScope.launch { refreshNow() }
    }

    private suspend fun refreshNow() {
        isBusy = true
        try {
            repo.sync()
        } catch (_: Exception) {
            // Cache already rendered; a failed background refresh just leaves it as-is.
        } finally {
            isBusy = false
        }
    }

    fun addFile(name: String, bytes: ByteArray, mimeType: String) {
        viewModelScope.launch { afterShare(repo.addFile(name, bytes, mimeType)) }
    }

    private suspend fun afterShare(result: ShareResult) {
        _toasts.tryEmit(
            if (result.queued) "Saved ${result.name} — will sync when the server is reachable" else "Synced ${result.name}",
        )
        // A share is also a fresh check: pull anything other devices did meanwhile.
        SyncScheduler.reset(getApplication())
        refreshNow()
    }

    /** Tap on a tile: notes open in the viewer, links in the browser, anything else in the app for its type. */
    fun open(item: FileItem) {
        viewModelScope.launch {
            when {
                item.isNote -> repo.openNote(item.id)?.let { page = Page.Viewing(it) }
                    ?: _toasts.tryEmit("Couldn't open ${item.name} — not on this device yet")
                item.isLink -> repo.linkUrl(item.id)?.takeIf { it.isNotBlank() }?.let { _openUrls.tryEmit(it) }
                else -> repo.openableFile(item)?.let { _openFiles.tryEmit(it) }
                    ?: _toasts.tryEmit("Couldn't open ${item.name} — not on this device yet")
            }
        }
    }

    fun newNote() {
        page = Page.Editing(null, Note(created = nowRfc3339()))
    }

    fun editNote(open: OpenNote) {
        page = Page.Editing(open, open.note)
    }

    /** Back: editor -> viewer (or the grid for a new note), viewer -> grid. */
    fun closePage() {
        page = when (val p = page) {
            is Page.Editing -> p.open?.let { Page.Viewing(it) }
            else -> null
        }
    }

    fun saveNote(edited: Note) {
        val editing = page as? Page.Editing ?: return
        viewModelScope.launch {
            isSaving = true
            try {
                val open = editing.open
                if (open == null) {
                    val result = repo.addNote(edited)
                    page = null
                    afterShare(result)
                } else if (edited.toToml() == open.note.toToml()) {
                    page = Page.Viewing(open)
                } else {
                    val saved = repo.saveNote(open, edited)
                    if (saved.result == "merged") _toasts.tryEmit("Saved — merged with changes from another device")
                    page = Page.Viewing(repo.openNote(open.item.id) ?: OpenNote(saved.entry, edited, saved.entry.head))
                }
            } catch (e: ConflictException) {
                noteConflict = NoteConflict(editing.open!!, edited)
            } catch (_: Exception) {
                _toasts.tryEmit("Couldn't save — server unreachable. Your edit is still open.")
            } finally {
                isSaving = false
            }
        }
    }

    /** Answer to a [NoteConflict]; null (dismissed) goes back to editing. */
    fun resolveNoteConflict(keep: Keep?) {
        val conflict = noteConflict ?: return
        noteConflict = null
        if (keep == null) return
        viewModelScope.launch {
            isSaving = true
            try {
                when (keep) {
                    // Theirs stays in the file's history either way.
                    Keep.MINE -> repo.saveNote(conflict.open, conflict.mine, force = true)
                    Keep.THEIRS -> Unit
                    Keep.BOTH -> repo.addNote(
                        conflict.mine.copy(title = conflict.mine.displayTitle + " (conflict copy)", created = nowRfc3339()),
                    )
                }
                refreshNow()
                page = repo.openNote(conflict.open.item.id)?.let { Page.Viewing(it) }
            } catch (_: Exception) {
                noteConflict = conflict
                _toasts.tryEmit("Couldn't save — server unreachable. Your edit is still open.")
            } finally {
                isSaving = false
            }
        }
    }

    fun reorder(orderedIds: List<String>) {
        viewModelScope.launch {
            try {
                repo.reorder(orderedIds)
            } catch (_: Exception) {
            }
        }
    }

    fun rename(id: String, newName: String) {
        viewModelScope.launch {
            try {
                repo.rename(id, newName)
            } catch (_: Exception) {
            }
        }
    }

    fun delete(id: String) {
        viewModelScope.launch {
            try {
                repo.delete(id)
            } catch (e: ConflictException) {
                if (e.current.deleted) {
                    refreshNow()
                } else {
                    deleteConflict = DeleteConflict(e.current)
                }
            } catch (_: Exception) {
                _toasts.tryEmit("Couldn't delete — server unreachable")
            }
        }
    }

    /** Answer to a [DeleteConflict]: delete the newer version anyway, or keep it. */
    fun resolveDeleteConflict(deleteAnyway: Boolean) {
        val conflict = deleteConflict ?: return
        deleteConflict = null
        viewModelScope.launch {
            try {
                if (deleteAnyway) repo.delete(conflict.item.id, force = true)
                refreshNow()
            } catch (_: Exception) {
                _toasts.tryEmit("Couldn't delete — server unreachable")
            }
        }
    }

    suspend fun contentBytes(id: String): ByteArray? = repo.contentBytes(id)

    companion object {
        private const val MIN_RETRY_MS = 60_000L
        private const val MAX_RETRY_MS = 60 * 60_000L
    }
}
