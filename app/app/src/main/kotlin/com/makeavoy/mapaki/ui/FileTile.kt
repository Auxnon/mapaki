package com.makeavoy.mapaki.ui

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.InsertDriveFile
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.material.icons.automirrored.filled.Notes
import androidx.compose.ui.text.font.FontWeight
import com.makeavoy.mapaki.data.model.FileItem
import com.makeavoy.mapaki.data.model.FileKind
import com.makeavoy.mapaki.data.model.isLink
import com.makeavoy.mapaki.data.model.isNote
import com.makeavoy.mapaki.data.notes.Images
import com.makeavoy.mapaki.data.notes.LinkCard
import com.makeavoy.mapaki.data.notes.Note

@Composable
fun FileTile(
    item: FileItem,
    thumbnailProvider: suspend (FileItem) -> ByteArray?,
    onOpen: () -> Unit,
    onExport: () -> Unit,
    onRename: (String) -> Unit,
    onDelete: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var menuExpanded by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf(false) }
    var renameText by remember(item.name) { mutableStateOf(item.name) }
    var bitmap by remember(item.id, item.hash) { mutableStateOf<androidx.compose.ui.graphics.ImageBitmap?>(null) }
    // Notes and links are parsed from the on-device copy for their tile.
    var note by remember(item.id, item.hash) { mutableStateOf<Note?>(null) }
    var link by remember(item.id, item.hash) { mutableStateOf<LinkCard?>(null) }

    LaunchedEffect(item.id, item.hash) {
        val wanted = item.kind == FileKind.IMAGE || item.isNote || item.isLink
        val bytes = if (wanted) thumbnailProvider(item) else null
        if (bytes != null) {
            val image = when {
                item.isNote -> Note.parse(bytes).also { note = it }.images.firstOrNull()?.data
                item.isLink -> LinkCard.parse(bytes).also { link = it }.preview?.data
                else -> bytes
            }
            bitmap = image?.let { Images.decodeSampled(it, 512)?.asImageBitmap() }
        }
    }

    val caption = when {
        item.isNote -> note?.displayTitle ?: item.name.removeSuffix(".note")
        item.isLink -> link?.displayTitle ?: item.name.removeSuffix(".link")
        else -> item.name
    }

    Card(onClick = onOpen, modifier = modifier.aspectRatio(1f).fillMaxWidth()) {
        Box(Modifier.fillMaxSize()) {
            when {
                bitmap != null -> Image(
                    bitmap = bitmap!!,
                    contentDescription = item.name,
                    modifier = Modifier.fillMaxSize(),
                    contentScale = ContentScale.Crop,
                )
                item.isNote && note != null -> NoteText(note!!)
                item.isNote -> TileIconLabel(Icons.AutoMirrored.Filled.Notes, caption)
                item.isLink -> TileIconLabel(Icons.Filled.Link, caption)
                else -> TileIconLabel(Icons.Filled.InsertDriveFile, item.name)
            }

            if (bitmap != null) {
                Column(
                    Modifier
                        .align(Alignment.BottomStart)
                        .fillMaxWidth()
                        .background(Color.Black.copy(alpha = 0.5f))
                        .padding(horizontal = 6.dp, vertical = 4.dp),
                ) {
                    Text(
                        text = caption,
                        color = Color.White,
                        maxLines = if (item.isLink) 2 else 1,
                        overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.labelSmall,
                    )
                    link?.site?.takeIf { it.isNotEmpty() }?.let {
                        Text(it, color = Color.White.copy(alpha = 0.75f), maxLines = 1, style = MaterialTheme.typography.labelSmall)
                    }
                }
            }

            Surface(
                modifier = Modifier.align(Alignment.TopEnd).padding(4.dp).size(28.dp),
                shape = CircleShape,
                color = Color.Black.copy(alpha = 0.35f),
            ) {
                IconButton(onClick = { menuExpanded = true }) {
                    Icon(Icons.Filled.MoreVert, contentDescription = "Options", tint = Color.White)
                }
            }
            DropdownMenu(expanded = menuExpanded, onDismissRequest = { menuExpanded = false }) {
                DropdownMenuItem(text = { Text("Export") }, onClick = {
                    menuExpanded = false
                    onExport()
                })
                DropdownMenuItem(text = { Text("Rename") }, onClick = {
                    menuExpanded = false
                    renaming = true
                })
                DropdownMenuItem(text = { Text("Delete") }, onClick = {
                    menuExpanded = false
                    onDelete()
                })
            }
        }
    }

    if (renaming) {
        AlertDialog(
            onDismissRequest = { renaming = false },
            title = { Text("Rename") },
            text = {
                OutlinedTextField(value = renameText, onValueChange = { renameText = it }, singleLine = true)
            },
            confirmButton = {
                TextButton(onClick = {
                    renaming = false
                    onRename(renameText)
                }) { Text("Save") }
            },
            dismissButton = {
                TextButton(onClick = { renaming = false }) { Text("Cancel") }
            },
        )
    }
}

@Composable
private fun NoteText(note: Note) {
    Column(Modifier.fillMaxSize().padding(10.dp)) {
        Text(
            note.displayTitle,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
            style = MaterialTheme.typography.labelLarge.copy(fontWeight = FontWeight.SemiBold),
        )
        Spacer(Modifier.height(4.dp))
        val preview = note.body.lines().filter { com.makeavoy.mapaki.data.notes.imageRef(it) == null }
            .dropWhile { it.isBlank() || it.trim() == note.displayTitle }
            .joinToString("\n")
        Text(preview, maxLines = 6, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall)
    }
}

@Composable
private fun TileIconLabel(icon: ImageVector, name: String) {
    Column(
        modifier = Modifier.fillMaxSize().padding(12.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = androidx.compose.foundation.layout.Arrangement.Center,
    ) {
        Icon(icon, contentDescription = null, modifier = Modifier.size(40.dp))
        Spacer(Modifier.height(6.dp))
        Text(
            name,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
            style = MaterialTheme.typography.labelSmall,
            textAlign = TextAlign.Center,
        )
    }
}
