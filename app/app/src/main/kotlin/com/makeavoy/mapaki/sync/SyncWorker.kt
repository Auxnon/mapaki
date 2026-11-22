package com.makeavoy.mapaki.sync

import android.content.Context
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import com.makeavoy.mapaki.data.Repository
import com.makeavoy.mapaki.data.store.SyncPrefs
import java.util.concurrent.TimeUnit

/**
 * Background checks while the app isn't open. Each check that finds nothing
 * quadruples the wait before the next one — 15 min, 1 h, 4 h, then stays at
 * 4 h — and anything changing (or opening the app, or sharing) snaps it back
 * to 15 min. Only runs on unmetered networks, so it never touches mobile data
 * and naturally pauses while away from Wi-Fi.
 */
object SyncScheduler {
    private const val WORK_NAME = "mapaki-background-sync"
    const val MIN_DELAY_MINUTES = 15L // WorkManager's floor for background work anyway
    const val MAX_DELAY_MINUTES = 4 * 60L
    private const val BACKOFF_FACTOR = 4

    fun nextDelay(currentMinutes: Long): Long =
        (currentMinutes.coerceAtLeast(MIN_DELAY_MINUTES) * BACKOFF_FACTOR).coerceAtMost(MAX_DELAY_MINUTES)

    /** Called on app open, share, or a check that found changes. */
    fun reset(context: Context) = schedule(context, MIN_DELAY_MINUTES)

    /**
     * [fromWorker]: the running check is itself the unique work, so REPLACE
     * would cancel it; append the next check behind it instead.
     */
    fun schedule(context: Context, delayMinutes: Long, fromWorker: Boolean = false) {
        SyncPrefs(context).backgroundDelayMinutes = delayMinutes
        val request = OneTimeWorkRequestBuilder<SyncWorker>()
            .setInitialDelay(delayMinutes, TimeUnit.MINUTES)
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.UNMETERED).build())
            .build()
        WorkManager.getInstance(context).enqueueUniqueWork(
            WORK_NAME,
            if (fromWorker) ExistingWorkPolicy.APPEND_OR_REPLACE else ExistingWorkPolicy.REPLACE,
            request,
        )
    }
}

class SyncWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        val repo = Repository(applicationContext)
        if (!repo.isPaired) return Result.success()
        val prefs = SyncPrefs(applicationContext)
        val changed = try {
            repo.sync()
        } catch (_: Exception) {
            false // unreachable counts as "nothing new" — back off the same way
        }
        val next = if (changed) SyncScheduler.MIN_DELAY_MINUTES else SyncScheduler.nextDelay(prefs.backgroundDelayMinutes)
        SyncScheduler.schedule(applicationContext, next, fromWorker = true)
        return Result.success()
    }
}
