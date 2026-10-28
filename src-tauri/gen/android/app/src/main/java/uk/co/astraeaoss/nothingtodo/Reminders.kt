package uk.co.astraeaoss.nothingtodo

import android.app.Activity
import android.app.AlarmManager
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * Hands reminders to Android so they fire with the app closed. Rust sends the full list of
 * upcoming reminders after every change; it is saved to a file so the reboot receiver can re-arm
 * alarms without starting the app's Rust side or webview.
 */
object Reminders {
  private const val FILE = "scheduled-reminders.json"
  private const val CHANNEL = "reminders"
  private const val EXTRA_ID = "id"
  private const val EXTRA_TITLE = "title"
  private const val EXTRA_AT = "at"

  data class Reminder(val id: Int, val at: Long, val title: String)

  @Synchronized
  fun replace(context: Context, reminders: List<Reminder>) {
    val keep = reminders.map { it.id }.toSet()
    load(context).filter { it.id !in keep }.forEach { cancel(context, it.id) }
    // Save before arming, so a reboot part-way through still restores everything.
    save(context, reminders)
    reminders.forEach { arm(context, it) }
  }

  /** After a reboot or app update, which clear alarms. Reminders already past are skipped. */
  @Synchronized
  fun rearm(context: Context) {
    load(context).forEach { arm(context, it) }
  }

  private fun arm(context: Context, reminder: Reminder) {
    if (reminder.at <= System.currentTimeMillis()) return
    val alarms = context.getSystemService(AlarmManager::class.java)
    val intent = Intent(context, ReminderReceiver::class.java)
      .putExtra(EXTRA_ID, reminder.id)
      .putExtra(EXTRA_TITLE, reminder.title)
      .putExtra(EXTRA_AT, reminder.at)
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
    val id = intent.getIntExtra(EXTRA_ID, 0)
    val title = intent.getStringExtra(EXTRA_TITLE) ?: return
    val notifications = context.getSystemService(NotificationManager::class.java)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      notifications.createNotificationChannel(
        NotificationChannel(CHANNEL, "Reminders", NotificationManager.IMPORTANCE_HIGH)
      )
    }
    val open = PendingIntent.getActivity(
      context,
      id,
      Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
    )
    val notification = NotificationCompat.Builder(context, CHANNEL)
      .setSmallIcon(android.R.drawable.ic_popup_reminder)
      .setContentTitle("NothingToDo reminder")
      .setContentText(title)
      .setWhen(intent.getLongExtra(EXTRA_AT, System.currentTimeMillis()))
      .setShowWhen(true)
      .setCategory(NotificationCompat.CATEGORY_REMINDER)
      .setPriority(NotificationCompat.PRIORITY_HIGH)
      .setContentIntent(open)
      .setAutoCancel(true)
      .build()
    try {
      NotificationManagerCompat.from(context).notify(id, notification)
    } catch (_: SecurityException) {
      // Notification permission was denied or revoked; the task still shows as overdue.
    }
  }

  private fun file(context: Context) = File(context.filesDir, FILE)

  private fun save(context: Context, reminders: List<Reminder>) {
    val json = JSONArray()
    reminders.forEach {
      json.put(JSONObject().put("id", it.id).put("at", it.at).put("title", it.title))
    }
    val target = file(context)
    val temporary = File(target.path + ".new")
    temporary.writeText(json.toString())
    temporary.renameTo(target)
  }

  private fun load(context: Context): List<Reminder> {
    val text = try {
      file(context).readText()
    } catch (_: Exception) {
      return emptyList()
    }
    return try {
      val json = JSONArray(text)
      (0 until json.length()).map {
        val item = json.getJSONObject(it)
        Reminder(item.getInt("id"), item.getLong("at"), item.getString("title"))
      }
    } catch (_: Exception) {
      emptyList()
    }
  }
}

class ReminderReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) = Reminders.show(context, intent)
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
}

@InvokeArg
class ScheduleArgs {
  var reminders: List<ScheduledReminder> = emptyList()
}

@TauriPlugin
class ReminderPlugin(private val activity: Activity) : Plugin(activity) {
  @Command
  fun schedule(invoke: Invoke) {
    val args = invoke.parseArgs(ScheduleArgs::class.java)
    Reminders.replace(activity.applicationContext, args.reminders.map { Reminders.Reminder(it.id, it.at, it.title) })
    invoke.resolve()
  }
}
