package uk.co.astraeaoss.nothingtodo

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.media.AudioAttributes
import android.media.MediaPlayer
import android.media.RingtoneManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.WindowManager
import android.widget.LinearLayout
import android.widget.TextView
import androidx.activity.ComponentActivity
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** 24-hour time, as in the app. */
fun clock(at: Long): String = SimpleDateFormat("HH:mm", Locale.getDefault()).format(Date(at))

/**
 * An alarm rings until it is stopped (roadmap 5a). The alarm-clock alarm armed in Reminders.kt
 * starts this foreground service, which loops the default alarm sound on the alarm stream,
 * vibrates and shows [AlarmActivity] full screen. Stop records a Complete (a one-off alarm is
 * done, a repeating one moves on) and Snooze re-arms it 10 minutes later, both through the same
 * action log as notification buttons. Unanswered, it stops after 10 minutes and leaves a
 * notification behind.
 */
class AlarmService : Service() {
  companion object {
    private const val CHANNEL = "alarms"
    const val STOP = "uk.co.astraeaoss.nothingtodo.ALARM_STOP"
    const val SNOOZE = "uk.co.astraeaoss.nothingtodo.ALARM_SNOOZE"
    private const val LIMIT_MILLIS = 10 * 60 * 1000L

    /** The alarm ringing now, for the alarm screen. */
    @Volatile var ringing: Reminders.Reminder? = null

    fun start(context: Context, reminder: Reminders.Reminder) {
      ContextCompat.startForegroundService(context, reminder.putInto(Intent(context, AlarmService::class.java)))
    }

    fun send(context: Context, action: String) {
      context.startService(Intent(context, AlarmService::class.java).setAction(action))
    }
  }

  private var player: MediaPlayer? = null
  private var vibrator: Vibrator? = null
  private val handler = Handler(Looper.getMainLooper())
  private val timeout = Runnable { giveUp() }

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    when (intent?.action) {
      STOP -> finish(Reminders.ACTION_COMPLETE)
      SNOOZE -> finish(Reminders.ACTION_SNOOZE)
      else -> {
        val reminder = intent?.let { Reminders.Reminder.fromIntent(it) }
        if (reminder == null) stopSelf() else ring(reminder)
      }
    }
    return START_NOT_STICKY
  }

  private fun ring(reminder: Reminders.Reminder) {
    // A second alarm while one rings replaces it; the first stays as a plain notification.
    ringing?.let { if (it.id != reminder.id) Reminders.show(this, it.putInto(Intent())) }
    stopSound()
    ringing = reminder
    val type = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
      // For apps holding USE_EXACT_ALARM that continue an alarm in the background.
      ServiceInfo.FOREGROUND_SERVICE_TYPE_SYSTEM_EXEMPTED
    } else 0
    ServiceCompat.startForeground(this, reminder.id, notification(reminder), type)
    startSound()
    startVibration()
    handler.removeCallbacks(timeout)
    handler.postDelayed(timeout, LIMIT_MILLIS)
    startActivity(Intent(this, AlarmActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
  }

  private fun notification(reminder: Reminders.Reminder): Notification {
    val manager = getSystemService(NotificationManager::class.java)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      manager.createNotificationChannel(NotificationChannel(CHANNEL, "Alarms", NotificationManager.IMPORTANCE_HIGH).apply {
        // The service plays the sound itself, on the alarm stream.
        setSound(null, null)
        enableVibration(false)
      })
    }
    val screen = PendingIntent.getActivity(
      this, reminder.id, Intent(this, AlarmActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
    )
    fun action(name: String) = PendingIntent.getService(
      this, name.hashCode(), Intent(this, AlarmService::class.java).setAction(name), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
    )
    // Without the full-screen permission (Android 14+ may withhold it) this shows as a heads-up
    // notification instead, and the alarm still rings.
    return NotificationCompat.Builder(this, CHANNEL)
      .setSmallIcon(android.R.drawable.ic_lock_idle_alarm)
      .setContentTitle("Alarm · ${clock(reminder.at)}")
      .setContentText(reminder.title)
      .setCategory(NotificationCompat.CATEGORY_ALARM)
      .setPriority(NotificationCompat.PRIORITY_MAX)
      .setOngoing(true)
      .setFullScreenIntent(screen, true)
      .setContentIntent(screen)
      .addAction(0, "Stop", action(STOP))
      .addAction(0, "Snooze 10 min", action(SNOOZE))
      .build()
  }

  private fun startSound() {
    val uri = RingtoneManager.getActualDefaultRingtoneUri(this, RingtoneManager.TYPE_ALARM)
      ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_ALARM)
      ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE)
    player = try {
      MediaPlayer().apply {
        setAudioAttributes(AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_ALARM).setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION).build())
        setDataSource(this@AlarmService, uri)
        isLooping = true
        prepare()
        start()
      }
    } catch (_: Exception) {
      null // No playable sound: the vibration and screen still alert.
    }
  }

  @Suppress("DEPRECATION")
  private fun startVibration() {
    vibrator = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
      getSystemService(VibratorManager::class.java).defaultVibrator
    } else {
      getSystemService(Vibrator::class.java)
    }
    val pattern = longArrayOf(0, 700, 500)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      vibrator?.vibrate(VibrationEffect.createWaveform(pattern, 0))
    } else {
      vibrator?.vibrate(pattern, 0)
    }
  }

  private fun stopSound() {
    player?.run { try { stop() } catch (_: Exception) {}; release() }
    player = null
    vibrator?.cancel()
    vibrator = null
  }

  private fun finish(action: String) {
    val reminder = ringing
    if (reminder != null) Reminders.record(this, reminder, action)
    end()
  }

  /** Nobody answered: stop ringing and leave a normal notification. */
  private fun giveUp() {
    ringing?.let { Reminders.show(this, it.copy(ring = false).putInto(Intent())) }
    end()
  }

  private fun end() {
    handler.removeCallbacks(timeout)
    stopSound()
    ringing = null
    AlarmActivity.current?.finish()
    ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
    stopSelf()
  }

  override fun onDestroy() {
    stopSound()
    super.onDestroy()
  }
}

/** The full-screen alarm, shown over the lock screen: hold to stop, or snooze. */
class AlarmActivity : ComponentActivity() {
  companion object {
    @Volatile var current: AlarmActivity? = null
    private const val HOLD_MILLIS = 900L
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    current = this
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
      setShowWhenLocked(true)
      setTurnScreenOn(true)
    } else {
      @Suppress("DEPRECATION")
      window.addFlags(WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON)
    }
    window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    val reminder = AlarmService.ringing
    if (reminder == null) {
      finish()
      return
    }
    setContentView(layout(reminder))
  }

  override fun onDestroy() {
    if (current == this) current = null
    super.onDestroy()
  }

  // Back does not dismiss a ringing alarm; stop or snooze it.
  @Deprecated("Back is handled by keeping the alarm up")
  override fun onBackPressed() {}

  private fun dp(value: Int) = (value * resources.displayMetrics.density).toInt()

  private fun text(value: String, size: Float, mono: Boolean, color: Int = Color.WHITE) = TextView(this).apply {
    text = value
    textSize = size
    setTextColor(color)
    gravity = Gravity.CENTER
    typeface = if (mono) Typeface.create(Typeface.MONOSPACE, Typeface.BOLD) else Typeface.DEFAULT
    if (mono) letterSpacing = 0.12f
  }

  private fun pill(label: String, filled: Boolean) = text(label, 14f, true, if (filled) Color.BLACK else Color.WHITE).apply {
    background = GradientDrawable().apply {
      cornerRadius = dp(99).toFloat()
      if (filled) setColor(Color.WHITE) else setStroke(dp(1), Color.WHITE)
    }
    setPadding(dp(24), dp(18), dp(24), dp(18))
  }

  private fun layout(reminder: Reminders.Reminder): View {
    val gray = Color.rgb(140, 140, 140)
    val root = LinearLayout(this).apply {
      orientation = LinearLayout.VERTICAL
      gravity = Gravity.CENTER
      setBackgroundColor(Color.BLACK)
      setPadding(dp(28), dp(48), dp(28), dp(48))
    }
    val dot = View(this).apply {
      background = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(Color.rgb(215, 25, 33)) }
    }
    root.addView(dot, LinearLayout.LayoutParams(dp(14), dp(14)).apply { bottomMargin = dp(20) })
    root.addView(text("ALARM", 13f, true, gray))
    root.addView(text(clock(reminder.at), 64f, true).apply { setPadding(0, dp(8), 0, dp(8)) })
    root.addView(text(reminder.title, 22f, false))
    root.addView(View(this), LinearLayout.LayoutParams(1, 0, 1f))
    val stop = pill("HOLD TO STOP", true)
    val hold = Handler(Looper.getMainLooper())
    val done = Runnable { AlarmService.send(this, AlarmService.STOP) }
    stop.setOnTouchListener { view, event ->
      when (event.action) {
        MotionEvent.ACTION_DOWN -> { (view as TextView).text = "HOLD…"; hold.postDelayed(done, HOLD_MILLIS) }
        MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> { (view as TextView).text = "HOLD TO STOP"; hold.removeCallbacks(done); view.performClick() }
      }
      true
    }
    val snooze = pill("SNOOZE 10 MIN", false)
    snooze.setOnClickListener { AlarmService.send(this, AlarmService.SNOOZE) }
    val full = LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT)
    root.addView(stop, full)
    root.addView(snooze, LinearLayout.LayoutParams(full).apply { topMargin = dp(12) })
    return root
  }
}
