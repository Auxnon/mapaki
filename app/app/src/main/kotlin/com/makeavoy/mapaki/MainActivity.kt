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
import com.makeavoy.mapaki.ui.MapakiScreen
import com.makeavoy.mapaki.ui.MapakiViewModel
import com.makeavoy.mapaki.ui.PairingScreen
import com.makeavoy.mapaki.ui.theme.MapakiTheme
import kotlinx.coroutines.launch

class MainActivity : ComponentActivity() {

    private val viewModel: MapakiViewModel by viewModels()
    private var pendingIntent by mutableStateOf<Intent?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        pendingIntent = intent

        setContent {
            MapakiTheme {
                AppRoot(
                    viewModel = viewModel,
                    incomingIntent = pendingIntent,
                    onIntentConsumed = { pendingIntent = null },
                )
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

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        pendingIntent = intent
    }
}

@Composable
private fun AppRoot(
    viewModel: MapakiViewModel,
    incomingIntent: Intent?,
    onIntentConsumed: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val items by viewModel.items.collectAsState()

    LaunchedEffect(Unit) {
        viewModel.toasts.collect { Toast.makeText(context, it, Toast.LENGTH_SHORT).show() }
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

    // Share-to-app intents (links and images).
    LaunchedEffect(incomingIntent) {
        val intent = incomingIntent ?: return@LaunchedEffect
        when (intent.action) {
            Intent.ACTION_SEND -> {
                @Suppress("DEPRECATION")
                val uri = intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM)
                val text = intent.getStringExtra(Intent.EXTRA_TEXT)
                when {
                    uri != null -> importUri(context, viewModel, uri)
                    text != null -> viewModel.addFile(nameForSharedText(text), text.toByteArray(), "text/plain")
                }
            }
            Intent.ACTION_SEND_MULTIPLE -> {
                @Suppress("DEPRECATION")
                val uris = intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)
                uris?.forEach { importUri(context, viewModel, it) }
            }
        }
        onIntentConsumed()
    }

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
    } else {
        MapakiScreen(
            items = items,
            isSyncing = viewModel.isBusy,
            thumbnailProvider = { item -> viewModel.contentBytes(item.id) },
            onReorder = viewModel::reorder,
            onImport = { importLauncher.launch(arrayOf("*/*")) },
            onExport = { item ->
                pendingExport = item
                exportLauncher.launch(item.name)
            },
            onRename = { item, newName -> viewModel.rename(item.id, newName) },
            onDelete = { item -> viewModel.delete(item.id) },
        )
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

/** A shared URL becomes a .link tile; any other shared text becomes a note. */
private fun nameForSharedText(text: String): String {
    val stamp = System.currentTimeMillis()
    val trimmed = text.trim()
    val isUrl = !trimmed.contains(' ') && !trimmed.contains('\n') &&
        (trimmed.startsWith("http://") || trimmed.startsWith("https://"))
    return if (isUrl) "link-$stamp.link" else "note-$stamp.txt"
}

private fun importUri(context: android.content.Context, viewModel: MapakiViewModel, uri: Uri) {
    val bytes = readAllBytes(context.contentResolver, uri) ?: return
    val mime = mimeTypeFor(context, uri)
    val name = displayNameFor(context, uri, fallback = "file-${System.currentTimeMillis()}")
    viewModel.addFile(name, bytes, mime)
}
