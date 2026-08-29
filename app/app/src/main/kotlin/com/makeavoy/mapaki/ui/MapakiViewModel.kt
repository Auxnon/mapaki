package com.makeavoy.mapaki.ui

import android.app.Application
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.makeavoy.mapaki.data.Repository
import com.makeavoy.mapaki.data.model.FileItem
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

class MapakiViewModel(app: Application) : AndroidViewModel(app) {
    private val repo = Repository(app)

    val items: StateFlow<List<FileItem>> = repo.items

    var isPaired by mutableStateOf(repo.isPaired)
        private set
    var isBusy by mutableStateOf(false)
        private set
    var pairError by mutableStateOf<String?>(null)
        private set

    init {
        if (isPaired) refresh()
    }

    fun pair(otp: String) {
        viewModelScope.launch {
            isBusy = true
            pairError = null
            try {
                repo.pair(otp)
                isPaired = true
            } catch (e: Exception) {
                pairError = e.message ?: "pairing failed"
            } finally {
                isBusy = false
            }
        }
    }

    fun refresh() {
        if (!repo.isPaired) return
        viewModelScope.launch {
            isBusy = true
            try {
                repo.sync()
            } catch (_: Exception) {
                // Cache already rendered; a failed background refresh just leaves it as-is.
            } finally {
                isBusy = false
            }
        }
    }

    fun addFile(name: String, bytes: ByteArray, mimeType: String) {
        viewModelScope.launch {
            try {
                repo.addFile(name, bytes, mimeType)
            } catch (_: Exception) {
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
            } catch (_: Exception) {
            }
        }
    }

    suspend fun contentBytes(id: String): ByteArray? = repo.contentBytes(id)
}
