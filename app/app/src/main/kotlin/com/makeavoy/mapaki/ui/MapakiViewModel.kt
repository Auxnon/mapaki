package com.makeavoy.mapaki.ui

import android.app.Application
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.makeavoy.mapaki.data.Repository
import com.makeavoy.mapaki.data.ShareResult
import com.makeavoy.mapaki.data.model.FileItem
import com.makeavoy.mapaki.data.net.ConflictException
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
        viewModelScope.launch {
            when (val result = repo.addFile(name, bytes, mimeType)) {
                is ShareResult.Synced -> _toasts.tryEmit("Synced ${result.name}")
                is ShareResult.Queued -> _toasts.tryEmit("Saved ${result.name} — will sync when the server is reachable")
            }
            // A share is also a fresh check: pull anything other devices did meanwhile.
            SyncScheduler.reset(getApplication())
            refreshNow()
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
