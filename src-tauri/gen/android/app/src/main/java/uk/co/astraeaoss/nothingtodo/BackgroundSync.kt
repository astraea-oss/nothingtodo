package uk.co.astraeaoss.nothingtodo

import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import android.util.Log
import org.json.JSONObject

/**
 * Background sync (`src-tauri/src/background.rs`): about every 15 minutes while there is a
 * network, even if the app is not opened for days, so changes from other devices reach the
 * alarms armed here. Android decides the exact timing and may stretch it in Doze, so this keeps
 * the phone roughly current; it is not instant.
 */
object BackgroundSync {
  private const val TAG = "NothingToDoSync"

  init {
    System.loadLibrary("tauri_app_lib")
  }

  /**
   * Guards the Android context the Rust side publishes for the keyring. The app's activity
   * publishes its own on creation and must not find one already there, so `MainActivity` takes
   * this lock around its creation too.
   */
  val lock = Any()

  /** An activity has been created in this process, so the app's Rust side runs here. */
  @Volatile var appStarted = false

  private external fun prepare(context: Context, packageName: String)
  private external fun run(dataDir: String, actions: String): String

  /** One pass: notification presses, sync, then re-arm. Returns false to retry later. */
  fun runOnce(context: Context): Boolean {
    val app = context.applicationContext
    val actions = Reminders.actions(app).toString()
    synchronized(lock) {
      if (!appStarted) prepare(app, app.packageName)
    }
    val result = try {
      JSONObject(run(app.dataDir.absolutePath, actions))
    } catch (e: Exception) {
      Log.w(TAG, "Background sync failed: $e")
      return false
    }
    if (result.has("error")) {
      Log.w(TAG, "Background sync failed: ${result.getString("error")}")
      return false
    }
    val acknowledged = result.getJSONArray("acknowledge")
    Reminders.acknowledge(app, (0 until acknowledged.length()).map { acknowledged.getString(it) }.toSet())
    val list = result.getJSONArray("reminders")
    Reminders.replace(app, (0 until list.length()).map { Reminders.Reminder.fromJson(list.getJSONObject(it)) })
    if (!result.isNull("syncError")) Log.w(TAG, "Sync failed: ${result.getString("syncError")}")
    else Log.i(TAG, "Background pass: synced=${result.getBoolean("synced")}, ${list.length()} armed, ${acknowledged.length()} presses applied")
    return true
  }
}

class SyncJob : JobService() {
  override fun onStartJob(params: JobParameters): Boolean {
    Thread {
      val done = try {
        BackgroundSync.runOnce(this)
      } catch (e: Throwable) {
        Log.w("NothingToDoSync", "Background sync stopped: $e")
        false
      }
      jobFinished(params, !done)
    }.start()
    return true
  }

  // Android stopped the job early (lost network, time limit): try again later.
  override fun onStopJob(params: JobParameters): Boolean = true

  companion object {
    private const val ID = 4100
    private const val PERIOD_MS = 15 * 60 * 1000L

    /** Idempotent; persisted by Android across reboots. */
    fun schedule(context: Context) {
      val jobs = context.getSystemService(JobScheduler::class.java) ?: return
      if (jobs.getPendingJob(ID) != null) return
      val job = JobInfo.Builder(ID, ComponentName(context, SyncJob::class.java))
        .setPeriodic(PERIOD_MS)
        .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY)
        .setPersisted(true)
        .build()
      // Called from app start and the reboot receiver: a refusal must never crash either.
      try {
        jobs.schedule(job)
      } catch (e: Exception) {
        Log.w("NothingToDoSync", "Could not schedule background sync: $e")
      }
    }
  }
}
