package com.makeavoy.mapaki.ui

import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.AddPhotoAlternate
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.makeavoy.mapaki.data.net.mimeTypeFor
import com.makeavoy.mapaki.data.net.readAllBytes
import com.makeavoy.mapaki.data.notes.EmbeddedImage
import com.makeavoy.mapaki.data.notes.Images
import com.makeavoy.mapaki.data.notes.Note
import com.makeavoy.mapaki.data.notes.imageRef
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Body text split into runs of text and the images placed between them. */
private sealed interface Block {
    data class Text(val text: String) : Block
    data class Picture(val image: EmbeddedImage?, val name: String) : Block
}

private fun blocks(note: Note): List<Block> {
    val out = mutableListOf<Block>()
    val text = StringBuilder()
    val placed = mutableSetOf<String>()
    fun flush() {
        if (text.isNotBlank()) out += Block.Text(text.toString().trimEnd('\n'))
        text.clear()
    }
    for (line in note.body.lines()) {
        val ref = imageRef(line)
        if (ref == null) {
            text.append(line).append('\n')
        } else {
            flush()
            placed += ref
            out += Block.Picture(note.images.firstOrNull { it.name == ref }, ref)
        }
    }
    flush()
    note.images.filter { it.name !in placed }.forEach { out += Block.Picture(it, it.name) }
    return out
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NoteViewerScreen(note: Note, onEdit: () -> Unit, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(note.displayTitle, maxLines = 1) },
                navigationIcon = {
                    IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back") }
                },
                actions = {
                    IconButton(onClick = onEdit) { Icon(Icons.Filled.Edit, contentDescription = "Edit") }
                },
            )
        },
    ) { padding ->
        SelectionContainer(Modifier.padding(padding)) {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                if (note.created.length >= 10) {
                    item { Text(note.created.take(10), style = MaterialTheme.typography.labelMedium) }
                }
                items(blocks(note)) { block ->
                    when (block) {
                        is Block.Text -> Text(block.text, style = MaterialTheme.typography.bodyLarge)
                        is Block.Picture -> NoteImage(block.image, block.name)
                    }
                }
            }
        }
    }
}

@Composable
private fun NoteImage(image: EmbeddedImage?, name: String) {
    val bitmap = remember(image) { image?.let { Images.decodeSampled(it.data, 1600)?.asImageBitmap() } }
    if (bitmap == null) {
        Text("[missing image: $name]", style = MaterialTheme.typography.labelMedium)
        return
    }
    Image(
        bitmap = bitmap,
        contentDescription = name,
        modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(8.dp)),
        contentScale = ContentScale.FillWidth,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NoteEditorScreen(
    initial: Note,
    isNew: Boolean,
    isSaving: Boolean,
    onSave: (Note) -> Unit,
    onCancel: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var title by remember(initial) { mutableStateOf(initial.title) }
    var body by remember(initial) { mutableStateOf(initial.body.trimEnd('\n')) }
    var images by remember(initial) { mutableStateOf(initial.images) }
    var adding by remember { mutableStateOf(false) }

    val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia()) { uris: List<Uri> ->
        if (uris.isEmpty()) return@rememberLauncherForActivityResult
        adding = true
        scope.launch {
            val added = withContext(Dispatchers.IO) {
                uris.mapNotNull { uri ->
                    val bytes = readAllBytes(context.contentResolver, uri) ?: return@mapNotNull null
                    Images.forNote(bytes, mimeTypeFor(context, uri))
                }
            }
            val taken = images.map { it.name }.toMutableSet()
            val refs = StringBuilder()
            val newImages = added.map { (bytes, mime) ->
                val ext = mime.substringAfter('/', "jpg").substringBefore('+').let { if (it == "jpeg") "jpg" else it }
                var n = taken.size + 1
                while ("image-$n.$ext" in taken) n++
                val name = "image-$n.$ext"
                taken += name
                refs.append("![](").append(name).append(")\n")
                EmbeddedImage(name, mime, bytes)
            }
            images = images + newImages
            if (refs.isNotEmpty()) body = (if (body.isEmpty()) "" else body.trimEnd('\n') + "\n") + refs.toString().trimEnd('\n')
            adding = false
        }
    }

    BackHandler(onBack = onCancel)
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(if (isNew) "New note" else "Edit note") },
                navigationIcon = {
                    IconButton(onClick = onCancel) { Icon(Icons.Filled.Close, contentDescription = "Discard") }
                },
                actions = {
                    if (isSaving || adding) {
                        CircularProgressIndicator(Modifier.padding(end = 12.dp).size(20.dp), strokeWidth = 2.dp)
                    }
                    IconButton(onClick = {
                        picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                    }) { Icon(Icons.Filled.AddPhotoAlternate, contentDescription = "Add image") }
                    TextButton(
                        enabled = !isSaving && !adding && (title.isNotBlank() || body.isNotBlank() || images.isNotEmpty()),
                        onClick = { onSave(initial.copy(title = title.trim(), body = body, images = images)) },
                    ) { Text("Save") }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier.padding(padding).fillMaxSize().imePadding().padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedTextField(
                value = title,
                onValueChange = { title = it },
                placeholder = { Text("Title") },
                singleLine = true,
                textStyle = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.SemiBold),
                modifier = Modifier.fillMaxWidth(),
            )
            if (images.isNotEmpty()) {
                Row(
                    Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    images.forEach { img ->
                        EditorThumb(img, onRemove = {
                            images = images.filter { it !== img }
                            // Drop the lines that placed it.
                            body = body.lines().filter { imageRef(it) != img.name }.joinToString("\n")
                        })
                    }
                }
            }
            OutlinedTextField(
                value = body,
                onValueChange = { body = it },
                placeholder = { Text("Write something… images show where their ![](name) line is") },
                modifier = Modifier.fillMaxWidth().weight(1f).heightIn(min = 200.dp).padding(bottom = 8.dp),
            )
        }
    }
}

@Composable
private fun EditorThumb(img: EmbeddedImage, onRemove: () -> Unit) {
    val bitmap = remember(img) { Images.decodeSampled(img.data, 256)?.asImageBitmap() }
    Box(Modifier.size(72.dp).clip(RoundedCornerShape(6.dp))) {
        if (bitmap != null) {
            Image(bitmap, contentDescription = img.name, modifier = Modifier.fillMaxSize(), contentScale = ContentScale.Crop)
        }
        Surface(
            modifier = Modifier.align(Alignment.TopEnd).padding(2.dp).size(22.dp),
            shape = CircleShape,
            color = Color.Black.copy(alpha = 0.5f),
        ) {
            IconButton(onClick = onRemove) {
                Icon(Icons.Filled.Close, contentDescription = "Remove ${img.name}", tint = Color.White, modifier = Modifier.size(14.dp))
            }
        }
    }
}
