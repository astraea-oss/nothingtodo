package uk.co.astraeaoss.nothingtodo

import android.os.Bundle
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // Android WebView reports zero CSS safe-area insets, so keep the page clear of the status
    // bar, navigation bar, display cutout and keyboard here instead.
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val bars = insets.getInsets(
        WindowInsetsCompat.Type.systemBars() or
          WindowInsetsCompat.Type.displayCutout() or
          WindowInsetsCompat.Type.ime()
      )
      view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      WindowInsetsCompat.CONSUMED
    }
    // Back closes whatever the page has open first (a sheet, an expanded row); src/main.ts
    // answers through window.nothingToDoBack. With nothing open, back leaves the app as usual.
    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        val webView = findWebView(content)
        if (webView == null) {
          leave()
          return
        }
        webView.evaluateJavascript("window.nothingToDoBack ? window.nothingToDoBack() : false") { handled ->
          if (handled != "true") leave()
        }
      }

      private fun leave() {
        isEnabled = false
        onBackPressedDispatcher.onBackPressed()
        isEnabled = true
      }
    })
  }

  private fun findWebView(view: View): WebView? {
    if (view is WebView) return view
    if (view is ViewGroup) {
      for (i in 0 until view.childCount) findWebView(view.getChildAt(i))?.let { return it }
    }
    return null
  }
}
