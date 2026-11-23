package com.makeavoy.mapaki

import android.content.Context
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.MutableTransitionState
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.spring
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Notes
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.CloudOff
import androidx.compose.material.icons.filled.ErrorOutline
import androidx.compose.material.icons.filled.InsertDriveFile
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.Upload
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ElevatedCard
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import com.makeavoy.mapaki.data.Repository
import com.makeavoy.mapaki.data.ShareResult
import com.makeavoy.mapaki.data.Sharing
import com.makeavoy.mapaki.data.net.displayNameFor
import com.makeavoy.mapaki.data.notes.Images
import com.makeavoy.mapaki.sync.SyncScheduler
import com.makeavoy.mapaki.ui.theme.MapakiTheme
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Receives shares without opening the app: a card slides up over whatever
 * app you shared from, shows what's being saved, and slides away on its
 * own. Tap the card to open mapaki, or anywhere else to dismiss it early;
 * the save carries on either way.
 */
class ShareActivity : ComponentActivity() {
    private val model: ShareModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        if (savedInstanceState == null) model.start(applicationContext, intent)
        setContent {
            MapakiTheme {
                ShareSheet(
                    state = model.state,
                    onOpen = {
                        startActivity(Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                        finish()
                    },
                    onGone = ::finish,
                )
            }
        }
    }
}

sealed interface ShareState {
    data class Saving(val title: String, val count: Int) : ShareState
    data class Done(val results: List<ShareResult>, val failed: Int) : ShareState
    data object NotPaired : ShareState
}

/** Outlives the card: a share started before the card is dismissed still finishes. */
private val shareScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

class ShareModel : ViewModel() {
    var state by mutableStateOf<ShareState>(ShareState.Saving("", 0))
        private set

    fun start(context: Context, intent: Intent) {
        val repo = Repository(context)
        if (!repo.isPaired) {
            state = ShareState.NotPaired
            return
        }
        val items = Sharing.itemsOf(intent)
        state = ShareState.Saving(guessTitle(context, items), items.size)
        shareScope.launch {
            val results = items.map { runCatching { Sharing.save(context, repo, it) }.getOrNull() }
            withContext(Dispatchers.Main) {
                state = ShareState.Done(results.filterNotNull(), results.count { it == null })
            }
            // A share is also a fresh check: send anything queued earlier, pull others' changes.
            SyncScheduler.reset(context)
            runCatching { repo.sync() }
        }
    }

    private fun guessTitle(context: Context, items: List<Sharing.Item>): String = when {
        items.size > 1 -> "${items.size} items"
        else -> when (val item = items.firstOrNull()) {
            is Sharing.Item.Text -> item.subject?.takeIf { it.isNotBlank() } ?: item.text.lineSequence().first().take(80)
            is Sharing.Item.Stream -> runCatching { displayNameFor(context, item.uri, fallback = "file") }.getOrDefault("file")
            null -> ""
        }
    }
}

private enum class Tone { WORKING, SAVED, QUEUED, PROBLEM }

@Composable
private fun ShareSheet(state: ShareState, onOpen: () -> Unit, onGone: () -> Unit) {
    val shown = remember { MutableTransitionState(false).apply { targetState = true } }
    var leaving by remember { mutableStateOf(false) }
    fun leave() {
        leaving = true
        shown.targetState = false
    }
    if (leaving && shown.isIdle && !shown.currentState) LaunchedEffect(Unit) { onGone() }

    LaunchedEffect(state) {
        val linger = when {
            state is ShareState.Done && state.failed == 0 && state.results.isNotEmpty() -> 2_800L
            state is ShareState.Saving -> return@LaunchedEffect
            else -> 5_000L
        }
        delay(linger)
        leave()
    }

    // The whole window is ours but see-through; a tap outside the card dismisses it.
    Box(
        Modifier.fillMaxSize().clickable(interactionSource = null, indication = null) { leave() },
        contentAlignment = Alignment.BottomCenter,
    ) {
        AnimatedVisibility(
            visibleState = shown,
            enter = slideInVertically(spring(dampingRatio = 0.75f, stiffness = Spring.StiffnessMediumLow)) { it } + fadeIn(),
            exit = slideOutVertically { it } + fadeOut(),
        ) {
            ShareCard(state, onOpen, Modifier.navigationBarsPadding().padding(16.dp))
        }
    }
}

@Composable
private fun ShareCard(state: ShareState, onOpen: () -> Unit, modifier: Modifier = Modifier) {
    val results = (state as? ShareState.Done)?.results.orEmpty()
    val tone = when (state) {
        is ShareState.Saving -> Tone.WORKING
        ShareState.NotPaired -> Tone.PROBLEM
        is ShareState.Done -> when {
            results.isEmpty() -> Tone.PROBLEM
            results.any { it.queued } -> Tone.QUEUED
            else -> Tone.SAVED
        }
    }
    val scheme = MaterialTheme.colorScheme
    val container by animateColorAsState(
        when (tone) {
            Tone.WORKING -> scheme.surfaceContainerHigh
            Tone.SAVED -> scheme.primaryContainer
            Tone.QUEUED -> scheme.tertiaryContainer
            Tone.PROBLEM -> scheme.errorContainer
        },
        label = "container",
    )
    val content by animateColorAsState(
        when (tone) {
            Tone.WORKING -> scheme.onSurface
            Tone.SAVED -> scheme.onPrimaryContainer
            Tone.QUEUED -> scheme.onTertiaryContainer
            Tone.PROBLEM -> scheme.onErrorContainer
        },
        label = "content",
    )

    val failed = (state as? ShareState.Done)?.failed ?: 0
    val status = when (state) {
        is ShareState.Saving -> "Saving to mapaki…"
        ShareState.NotPaired -> "mapaki isn't paired yet"
        is ShareState.Done -> when {
            results.isEmpty() -> "Couldn't read what was shared"
            tone == Tone.QUEUED -> "Saved on this phone · syncs when the server is reachable"
            failed > 0 -> "Saved · $failed couldn't be read"
            else -> "Saved to mapaki"
        }
    }
    val title = when (state) {
        is ShareState.Saving -> state.title
        ShareState.NotPaired -> "Tap to open mapaki and pair"
        is ShareState.Done -> if (results.size == 1) results[0].title else "${results.size} items"
    }
    val detail = when {
        results.size == 1 -> results[0].detail
        results.size > 1 -> results.joinToString(", ") { it.name }
        else -> ""
    }
    val first = results.firstOrNull()
    val image = results.firstNotNullOfOrNull { it.image }
    val icon = when {
        state is ShareState.Saving -> Icons.Filled.Upload
        first?.name?.endsWith(".link") == true -> Icons.Filled.Link
        first?.name?.endsWith(".note") == true -> Icons.AutoMirrored.Filled.Notes
        else -> Icons.Filled.InsertDriveFile
    }

    ElevatedCard(
        onClick = onOpen,
        shape = RoundedCornerShape(24.dp),
        colors = CardDefaults.elevatedCardColors(containerColor = container, contentColor = content),
        elevation = CardDefaults.elevatedCardElevation(defaultElevation = 8.dp),
        modifier = modifier.fillMaxWidth().widthIn(max = 520.dp),
    ) {
        Row(Modifier.padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
            Thumb(image, icon, content)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(status, style = MaterialTheme.typography.labelMedium, color = content.copy(alpha = 0.8f), maxLines = 2)
                if (title.isNotBlank()) {
                    Spacer(Modifier.height(2.dp))
                    Text(
                        title,
                        style = MaterialTheme.typography.titleSmall.copy(fontWeight = FontWeight.SemiBold),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                if (detail.isNotBlank()) {
                    Text(
                        detail,
                        style = MaterialTheme.typography.bodySmall,
                        color = content.copy(alpha = 0.8f),
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            Spacer(Modifier.width(8.dp))
            AnimatedContent(
                targetState = tone,
                transitionSpec = { (fadeIn() + scaleIn(initialScale = 0.6f)) togetherWith fadeOut() },
                label = "badge",
            ) { t -> Badge(t, content) }
        }
    }
}

@Composable
private fun Thumb(image: ByteArray?, icon: ImageVector, tint: Color) {
    val bitmap = remember(image) { image?.let { Images.decodeSampled(it, 192)?.asImageBitmap() } }
    Box(
        Modifier.size(56.dp).clip(RoundedCornerShape(14.dp)).background(tint.copy(alpha = 0.12f)),
        contentAlignment = Alignment.Center,
    ) {
        if (bitmap != null) {
            Image(bitmap, contentDescription = null, modifier = Modifier.fillMaxSize(), contentScale = ContentScale.Crop)
        } else {
            Icon(icon, contentDescription = null, modifier = Modifier.size(28.dp))
        }
    }
}

@Composable
private fun Badge(tone: Tone, tint: Color) {
    Box(Modifier.size(36.dp), contentAlignment = Alignment.Center) {
        when (tone) {
            Tone.WORKING -> CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.5.dp, color = tint)
            Tone.SAVED -> {
                // A little pop when it lands.
                val pop = remember { Animatable(0.4f) }
                LaunchedEffect(Unit) { pop.animateTo(1f, spring(dampingRatio = 0.4f, stiffness = Spring.StiffnessMedium)) }
                Box(
                    Modifier.size(32.dp).scale(pop.value).clip(CircleShape).background(MaterialTheme.colorScheme.primary),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(Icons.Filled.Check, contentDescription = "Saved", tint = MaterialTheme.colorScheme.onPrimary, modifier = Modifier.size(20.dp))
                }
            }
            Tone.QUEUED -> Icon(Icons.Filled.CloudOff, contentDescription = "Waiting to sync", tint = tint)
            Tone.PROBLEM -> Icon(Icons.Filled.ErrorOutline, contentDescription = "Problem", tint = tint)
        }
    }
}
