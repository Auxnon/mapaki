package com.makeavoy.mapaki

import android.Manifest
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import com.makeavoy.mapaki.data.model.FileItem
import com.makeavoy.mapaki.data.net.displayNameFor
import com.makeavoy.mapaki.data.net.mimeTypeFor
import com.makeavoy.mapaki.data.net.readAllBytes
import com.makeavoy.mapaki.ui.Keep
import com.makeavoy.mapaki.ui.MapakiScreen
import com.makeavoy.mapaki.ui.NoteEditorScreen
import com.makeavoy.mapaki.ui.NoteViewerScreen
import com.makeavoy.mapaki.ui.Page
import com.makeavoy.mapaki.ui.MapakiViewModel
import com.makeavoy.mapaki.ui.PairingScreen
import com.makeavoy.mapaki.ui.theme.MapakiTheme
import kotlinx.coroutines.launch

class MainActivity : ComponentActivity() {

    private val viewModel: MapakiViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MapakiTheme {
                AppRoot(viewModel = viewModel)
            }
        }
    }

    override fun onStart() {
        super.onStart()
        // Every app open is a fresh check and sync, then live updates while visible.
        viewModel.onForeground()
    }

    override fun onStop() {
        super.onStop()
        viewModel.onBackground()
    }
}

@Composable
private fun AppRoot(viewModel: MapakiViewModel) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val items by viewModel.items.collectAsState()

    LaunchedEffect(Unit) {
        viewModel.toasts.collect { Toast.makeText(context, it, Toast.LENGTH_SHORT).show() }
    }
    LaunchedEffect(Unit) {
        viewModel.openFiles.collect { file -> openWithApp(context, file) }
    }
    LaunchedEffect(Unit) {
        viewModel.openUrls.collect { url ->
            runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
                .onFailure { Toast.makeText(context, "No app can open $url", Toast.LENGTH_SHORT).show() }
        }
    }

    // The SSID is location data on every Android version, so this permission is what makes
    // network-mode detection work at all. Bump a tick on the result so the pairing screen's
    // "Connecting to: ..." line (a plain getter, not Compose State) re-reads once it's granted,
    // instead of only updating on the next unrelated recomposition.
    var permissionTick by remember { mutableStateOf(0) }
    val permissionLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        permissionTick++
    }
    LaunchedEffect(Unit) { permissionLauncher.launch(Manifest.permission.ACCESS_FINE_LOCATION) }
    val targetDescription = remember(permissionTick, viewModel.isPaired) { viewModel.targetDescription }

    val importLauncher = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        uris.forEach { uri -> importUri(context, viewModel, uri) }
    }

    var pendingExport by remember { mutableStateOf<FileItem?>(null) }
    val exportLauncher = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("*/*")) { uri ->
        val item = pendingExport
        pendingExport = null
        if (uri != null && item != null) {
            scope.launch {
                val bytes = viewModel.contentBytes(item.id)
                if (bytes != null) {
                    context.contentResolver.openOutputStream(uri)?.use { it.write(bytes) }
                }
            }
        }
    }

    if (!viewModel.isPaired) {
        PairingScreen(
            error = viewModel.pairError,
            isSubmitting = viewModel.isBusy,
            targetDescription = targetDescription,
            onSubmit = { code -> viewModel.pair(code) },
        )
    } else when (val page = viewModel.page) {
        is Page.Viewing -> NoteViewerScreen(
            note = page.open.note,
            onEdit = { viewModel.editNote(page.open) },
            onBack = viewModel::closePage,
        )
        is Page.Editing -> NoteEditorScreen(
            initial = page.draft,
            isNew = page.open == null,
            isSaving = viewModel.isSaving,
            onSave = viewModel::saveNote,
            onCancel = viewModel::closePage,
        )
        null -> MapakiScreen(
            items = items,
            isSyncing = viewModel.isBusy,
            thumbnailProvider = { item -> viewModel.contentBytes(item.id) },
            onReorder = viewModel::reorder,
            onImport = { importLauncher.launch(arrayOf("*/*")) },
            onNewNote = viewModel::newNote,
            onOpen = viewModel::open,
            onExport = { item ->
                pendingExport = item
                exportLauncher.launch(item.name)
            },
            onRename = { item, newName -> viewModel.rename(item.id, newName) },
            onDelete = { item -> viewModel.delete(item.id) },
        )
    }
    if (viewModel.isPaired) {
        viewModel.noteConflict?.let { conflict ->
            AlertDialog(
                onDismissRequest = { viewModel.resolveNoteConflict(null) },
                title = { Text("Changed on another device") },
                text = {
                    Text(
                        "\"${conflict.open.note.displayTitle}\" was edited elsewhere too, and the changes overlap. " +
                            "Whichever you don't keep stays in the note's history.",
                    )
                },
                confirmButton = {
                    TextButton(onClick = { viewModel.resolveNoteConflict(Keep.MINE) }) { Text("Keep mine") }
                },
                dismissButton = {
                    androidx.compose.foundation.layout.Row {
                        TextButton(onClick = { viewModel.resolveNoteConflict(Keep.BOTH) }) { Text("Keep both") }
                        TextButton(onClick = { viewModel.resolveNoteConflict(Keep.THEIRS) }) { Text("Keep theirs") }
                    }
                },
            )
        }
        viewModel.deleteConflict?.let { conflict ->
            AlertDialog(
                onDismissRequest = { viewModel.resolveDeleteConflict(deleteAnyway = false) },
                title = { Text("Changed on another device") },
                text = { Text("\"${conflict.item.name}\" was edited elsewhere after you last synced it. Delete the newer version anyway?") },
                confirmButton = {
                    TextButton(onClick = { viewModel.resolveDeleteConflict(deleteAnyway = true) }) { Text("Delete anyway") }
                },
                dismissButton = {
                    TextButton(onClick = { viewModel.resolveDeleteConflict(deleteAnyway = false) }) { Text("Keep it") }
                },
            )
        }
    }
}

private fun importUri(context: android.content.Context, viewModel: MapakiViewModel, uri: Uri) {
    val bytes = readAllBytes(context.contentResolver, uri) ?: return
    val mime = mimeTypeFor(context, uri)
    val name = displayNameFor(context, uri, fallback = "file-${System.currentTimeMillis()}")
    viewModel.addFile(name, bytes, mime)
}

/**
 * Hands a file to whichever app handles its type: the package installer for
 * an .apk, a PDF reader, the gallery... Android asks once before mapaki may
 * start an install.
 */
private fun openWithApp(context: android.content.Context, file: java.io.File) {
    val ext = file.extension.lowercase()
    val mime = android.webkit.MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext)
        ?: if (ext == "apk") "application/vnd.android.package-archive" else "application/octet-stream"
    val uri = androidx.core.content.FileProvider.getUriForFile(context, "${context.packageName}.files", file)
    val view = Intent(Intent.ACTION_VIEW).setDataAndType(uri, mime).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    try {
        context.startActivity(view)
    } catch (_: android.content.ActivityNotFoundException) {
        Toast.makeText(context, "No app here opens .$ext files — use Export to save it instead", Toast.LENGTH_LONG).show()
    }
}
