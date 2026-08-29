package com.makeavoy.mapaki.ui

import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshots.SnapshotStateList
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.boundsInParent
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.unit.dp
import androidx.compose.ui.zIndex
import com.makeavoy.mapaki.data.model.FileItem

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MapakiScreen(
    items: List<FileItem>,
    isSyncing: Boolean,
    thumbnailProvider: suspend (FileItem) -> ByteArray?,
    onReorder: (List<String>) -> Unit,
    onImport: () -> Unit,
    onExport: (FileItem) -> Unit,
    onRename: (FileItem, String) -> Unit,
    onDelete: (FileItem) -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Mapaki") },
                actions = {
                    if (isSyncing) {
                        CircularProgressIndicator(
                            modifier = Modifier.padding(end = 12.dp).size(20.dp),
                            strokeWidth = 2.dp,
                        )
                    }
                    IconButton(onClick = onImport) {
                        Icon(Icons.Filled.Add, contentDescription = "Import")
                    }
                },
            )
        },
    ) { padding ->
        if (items.isEmpty()) {
            Box(Modifier.padding(padding).fillMaxSize()) {
                Text(
                    "Nothing here yet — share a link or image to Mapaki, or tap + to import.",
                    modifier = Modifier.padding(32.dp),
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        } else {
            ReorderableFileGrid(
                items = items,
                modifier = Modifier.padding(padding).fillMaxSize(),
                thumbnailProvider = thumbnailProvider,
                onReorder = onReorder,
                onExport = onExport,
                onRename = onRename,
                onDelete = onDelete,
            )
        }
    }
}

@Composable
private fun ReorderableFileGrid(
    items: List<FileItem>,
    modifier: Modifier,
    thumbnailProvider: suspend (FileItem) -> ByteArray?,
    onReorder: (List<String>) -> Unit,
    onExport: (FileItem) -> Unit,
    onRename: (FileItem, String) -> Unit,
    onDelete: (FileItem) -> Unit,
) {
    val orderState = remember { SnapshotStateList<FileItem>() }
    var draggingId by remember { mutableStateOf<String?>(null) }
    var dragOffset by remember { mutableStateOf(Offset.Zero) }
    val bounds = remember { mutableStateMapOf<String, Rect>() }

    LaunchedEffect(items) {
        if (draggingId == null) {
            orderState.clear()
            orderState.addAll(items)
        }
    }

    LazyVerticalGrid(
        columns = GridCells.Adaptive(minSize = 128.dp),
        modifier = modifier,
        contentPadding = PaddingValues(12.dp),
        horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(10.dp),
        verticalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(10.dp),
    ) {
        items(items = orderState, key = { it.id }) { item ->
            val isDragging = item.id == draggingId
            FileTile(
                item = item,
                thumbnailProvider = thumbnailProvider,
                onExport = { onExport(item) },
                onRename = { newName -> onRename(item, newName) },
                onDelete = { onDelete(item) },
                modifier = Modifier
                    .animateItem()
                    .onGloballyPositioned { coords -> bounds[item.id] = coords.boundsInParent() }
                    .zIndex(if (isDragging) 1f else 0f)
                    .graphicsLayer {
                        if (isDragging) {
                            translationX = dragOffset.x
                            translationY = dragOffset.y
                            shadowElevation = 12f
                            scaleX = 1.04f
                            scaleY = 1.04f
                        }
                    }
                    .pointerInput(item.id) {
                        detectDragGesturesAfterLongPress(
                            onDragStart = {
                                draggingId = item.id
                                dragOffset = Offset.Zero
                            },
                            onDrag = { change, delta ->
                                change.consume()
                                dragOffset += delta
                                val myBounds = bounds[item.id] ?: return@detectDragGesturesAfterLongPress
                                val draggedCenter = myBounds.translate(dragOffset).center
                                val fromIndex = orderState.indexOfFirst { it.id == item.id }
                                val targetIndex = orderState.indices.minByOrNull { idx ->
                                    val b = bounds[orderState[idx].id]
                                    if (b == null) Float.MAX_VALUE else (b.center - draggedCenter).getDistanceSquared()
                                }
                                if (targetIndex != null && targetIndex != fromIndex && fromIndex >= 0) {
                                    val moved = orderState.removeAt(fromIndex)
                                    orderState.add(targetIndex, moved)
                                }
                            },
                            onDragEnd = {
                                draggingId = null
                                dragOffset = Offset.Zero
                                onReorder(orderState.map { it.id })
                            },
                            onDragCancel = {
                                draggingId = null
                                dragOffset = Offset.Zero
                            },
                        )
                    },
            )
        }
    }
}
