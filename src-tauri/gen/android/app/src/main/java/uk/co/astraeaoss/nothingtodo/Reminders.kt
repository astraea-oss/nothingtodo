package uk.co.astraeaoss.nothingtodo

import android.app.Activity
import android.app.AlarmManager
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.ContentValues
import android.content.Intent
import android.net.Uri
import android.os.Environment
import android.provider.MediaStore
import android.os.Build
import android.provider.Settings
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.util.UUID

/**
 * Hands reminders to Android so they fire with the app closed. Rust sends the full list of
 * upcoming reminders after every change; it is saved to a file so the reboot receiver can re-arm
 * alarms without starting the app's Rust side or webview.
 *
 * Notification buttons never touch the database. They act on the notification and alarm at once
 * and append to an action log, which the Rust side applies to the store, then acknowledges, the
 * next time it runs. Applying an action twice is harmless.
 */
object Reminders {
  private const val SCHEDULE_FILE = "scheduled-reminders.json"
  private const val ACTIONS_FILE = "reminder-actions.json"
  private const val CHANNEL = "reminders"
  private const val SNOOZE_MILLIS = 10 * 60 * 1000L
  private const val ACTION_COMPLETE = "uk.co.astraeaoss.nothingtodo.COMPLETE"
  private const val ACTION_SNOOZE = "uk.co.astraeaoss.nothingtodo.SNOOZE"

  /** `remindAt` is the synced reminder time; `at` can be later after a snooze. */
  data class Reminder(val id: Int, val at: Long, val title: String, val taskId: String, val remindAt: Long) {
    fun toJson(): JSONObject = JSONObject()
      .put("id", id).put("at", at).put("title", title).put("taskId", taskId).put("remindAt", remindAt)

    companion object {
      fun fromJson(json: JSONObject) = Reminder(
        json.getInt("id"), json.getLong("at"), json.getString("title"),
        json.getString("taskId"), json.getLong("remindAt")
      )

      fun fromIntent(intent: Intent): Reminder? {
        val title = intent.getStringExtra("title") ?: return null
        val taskId = intent.getStringExtra("taskId") ?: return null
        return Reminder(
          intent.getIntExtra("id", 0), intent.getLongExtra("at", 0), title, taskId,
          intent.getLongExtra("remindAt", 0)
        )
      }
    }

    fun putInto(intent: Intent): Intent = intent
      .putExtra("id", id).putExtra("at", at).putExtra("title", title)
      .putExtra("taskId", taskId).putExtra("remindAt", remindAt)
  }

  @Synchronized
  fun replace(context: Context, reminders: List<Reminder>) {
    // A snooze pressed since Rust last read the action log is not in its list yet; keep it.
    val listed = reminders.map { it.id }.toSet()
    val snoozed = pendingSnoozes(context).filter { it.id !in listed }
    val all = reminders + snoozed
    val keep = all.map { it.id }.toSet()
    loadSchedule(context).filter { it.id !in keep }.forEach { cancel(context, it.id) }
    // Save before arming, so a reboot part-way through still restores everything.
    saveSchedule(context, all)
    all.forEach { arm(context, it) }
  }

  /** After a reboot or app update, which clear alarms. Reminders already past are skipped. */
  @Synchronized
  fun rearm(context: Context) {
    loadSchedule(context).forEach { arm(context, it) }
  }

  private fun arm(context: Context, reminder: Reminder) {
    if (reminder.at <= System.currentTimeMillis()) return
    val alarms = context.getSystemService(AlarmManager::class.java)
    val intent = reminder.putInto(Intent(context, ReminderReceiver::class.java))
    val pending = PendingIntent.getBroadcast(
      context, reminder.id, intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
    )
    // Exact and able to wake the phone when exact alarms are allowed (USE_EXACT_ALARM, or
    // SCHEDULE_EXACT_ALARM on Android 12); otherwise Android delivers it inexactly.
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S || alarms.canScheduleExactAlarms()) {
      alarms.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, reminder.at, pending)
    } else {
      alarms.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, reminder.at, pending)
    }
  }

  private fun cancel(context: Context, id: Int) {
    val intent = Intent(context, ReminderReceiver::class.java)
    val pending = PendingIntent.getBroadcast(
      context, id, intent, PendingIntent.FLAG_NO_CREATE or PendingIntent.FLAG_IMMUTABLE
    ) ?: return
    context.getSystemService(AlarmManager::class.java).cancel(pending)
    pending.cancel()
  }

  fun show(context: Context, intent: Intent) {
    val reminder = Reminder.fromIntent(intent) ?: return
    val notifications = context.getSystemService(NotificationManager::class.java)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      notifications.createNotificationChannel(
        NotificationChannel(CHANNEL, "Reminders", NotificationManager.IMPORTANCE_HIGH)
      )
    }
    val open = PendingIntent.getActivity(
      context,
      reminder.id,
      Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
    )
    val notification = NotificationCompat.Builder(context, CHANNEL)
      .setSmallIcon(android.R.drawable.ic_popup_reminder)
      .setContentTitle("NothingToDo reminder")
      .setContentText(reminder.title)
      .setWhen(reminder.at)
      .setShowWhen(true)
      .setCategory(NotificationCompat.CATEGORY_REMINDER)
      .setPriority(NotificationCompat.PRIORITY_HIGH)
      .setContentIntent(open)
      .setAutoCancel(true)
      .addAction(0, "Complete", button(context, reminder, ACTION_COMPLETE))
      .addAction(0, "Snooze 10 min", button(context, reminder, ACTION_SNOOZE))
      .build()
    try {
      NotificationManagerCompat.from(context).notify(reminder.id, notification)
    } catch (_: SecurityException) {
      // Notification permission was denied or revoked; the task still shows as overdue.
    }
  }

  private fun button(context: Context, reminder: Reminder, action: String): PendingIntent {
    // The action string keeps the two buttons' intents distinct for the same notification ID.
    val intent = reminder.putInto(Intent(context, ReminderActionReceiver::class.java).setAction(action))
    return PendingIntent.getBroadcast(
      context, reminder.id, intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
    )
  }

  @Synchronized
  fun press(context: Context, intent: Intent) {
    val reminder = Reminder.fromIntent(intent) ?: return
    NotificationManagerCompat.from(context).cancel(reminder.id)
    val entry = JSONObject()
      .put("key", UUID.randomUUID().toString())
      .put("id", reminder.id)
      .put("title", reminder.title)
      .put("taskId", reminder.taskId)
      .put("remindAt", reminder.remindAt)
    val schedule = loadSchedule(context).filter { it.id != reminder.id }
    when (intent.action) {
      ACTION_COMPLETE -> {
        entry.put("action", "complete")
        cancel(context, reminder.id)
        saveSchedule(context, schedule)
      }
      ACTION_SNOOZE -> {
        val until = System.currentTimeMillis() + SNOOZE_MILLIS
        entry.put("action", "snooze").put("until", until)
        val snoozed = reminder.copy(at = until)
        saveSchedule(context, schedule + snoozed)
        arm(context, snoozed)
      }
      else -> return
    }
    val actions = loadActions(context)
    actions.put(entry)
    write(context, ACTIONS_FILE, actions.toString())
  }

  /** Every action not yet acknowledged by the Rust side. */
  @Synchronized
  fun actions(context: Context): JSONArray = loadActions(context)

  @Synchronized
  fun acknowledge(context: Context, keys: Set<String>) {
    val remaining = JSONArray()
    val actions = loadActions(context)
    for (i in 0 until actions.length()) {
      val action = actions.getJSONObject(i)
      if (action.getString("key") !in keys) remaining.put(action)
    }
    write(context, ACTIONS_FILE, remaining.toString())
  }

  private fun pendingSnoozes(context: Context): List<Reminder> {
    val actions = loadActions(context)
    return (0 until actions.length()).map { actions.getJSONObject(it) }
      .filter { it.optString("action") == "snooze" }
      .map {
        Reminder(it.getInt("id"), it.getLong("until"), it.getString("title"), it.getString("taskId"), it.getLong("remindAt"))
      }
  }

  private fun write(context: Context, name: String, text: String) {
    val target = File(context.filesDir, name)
    val temporary = File(target.path + ".new")
    temporary.writeText(text)
    temporary.renameTo(target)
  }

  private fun read(context: Context, name: String): String? = try {
    File(context.filesDir, name).readText()
  } catch (_: Exception) {
    null
  }

  private fun saveSchedule(context: Context, reminders: List<Reminder>) {
    val json = JSONArray()
    reminders.forEach { json.put(it.toJson()) }
    write(context, SCHEDULE_FILE, json.toString())
  }

  private fun loadSchedule(context: Context): List<Reminder> = try {
    val json = JSONArray(read(context, SCHEDULE_FILE) ?: "[]")
    (0 until json.length()).map { Reminder.fromJson(json.getJSONObject(it)) }
  } catch (_: Exception) {
    emptyList()
  }

  private fun loadActions(context: Context): JSONArray = try {
    JSONArray(read(context, ACTIONS_FILE) ?: "[]")
  } catch (_: Exception) {
    JSONArray()
  }
}

class ReminderReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) = Reminders.show(context, intent)
}

class ReminderActionReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) = Reminders.press(context, intent)
}

/** Alarms are cleared by a reboot, an app update and a change to exact-alarm access. */
class ReminderRestoreReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) = Reminders.rearm(context)
}

@InvokeArg
class ScheduledReminder {
  var id: Int = 0
  var at: Long = 0
  var title: String = ""
  var taskId: String = ""
  var remindAt: Long = 0
}

@InvokeArg
class ScheduleArgs {
  var reminders: List<ScheduledReminder> = emptyList()
}

@InvokeArg
class SaveArgs {
  var name: String = ""
  var text: String = ""
}

@InvokeArg
class AcknowledgeArgs {
  var keys: List<String> = emptyList()
}

@TauriPlugin
class ReminderPlugin(private val activity: Activity) : Plugin(activity) {
  @Command
  fun schedule(invoke: Invoke) {
    val args = invoke.parseArgs(ScheduleArgs::class.java)
    Reminders.replace(
      activity.applicationContext,
      args.reminders.map { Reminders.Reminder(it.id, it.at, it.title, it.taskId, it.remindAt) }
    )
    invoke.resolve()
  }

  @Command
  fun actions(invoke: Invoke) {
    invoke.resolve(JSObject().put("actions", Reminders.actions(activity.applicationContext)))
  }

  @Command
  fun acknowledgeActions(invoke: Invoke) {
    val args = invoke.parseArgs(AcknowledgeArgs::class.java)
    Reminders.acknowledge(activity.applicationContext, args.keys.toSet())
    invoke.resolve()
  }

  /** Saves a backup where the user can find it: Downloads on Android 10+, no permission needed. */
  @Command
  fun saveToDownloads(invoke: Invoke) {
    val args = invoke.parseArgs(SaveArgs::class.java)
    val context = activity.applicationContext
    val location = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
      val values = ContentValues().apply {
        put(MediaStore.Downloads.DISPLAY_NAME, args.name)
        put(MediaStore.Downloads.MIME_TYPE, "application/json")
      }
      val uri = context.contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
      if (uri == null) {
        invoke.reject("Could not create the file in Downloads.")
        return
      }
      context.contentResolver.openOutputStream(uri)?.use { it.write(args.text.toByteArray()) }
      "Downloads/${args.name}"
    } else {
      val folder = context.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS)
      val file = File(folder, args.name)
      file.writeText(args.text)
      file.path
    }
    invoke.resolve(JSObject().put("location", location))
  }

  /** Android stops showing the permission prompt after a refusal; settings can still allow it. */
  @Command
  fun openNotificationSettings(invoke: Invoke) {
    val intent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
        .putExtra(Settings.EXTRA_APP_PACKAGE, activity.packageName)
    } else {
      Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", activity.packageName, null))
    }
    activity.startActivity(intent)
    invoke.resolve()
  }
}
