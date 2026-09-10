package com.johnalindogan.slideshowpro.tv

import android.annotation.SuppressLint
import android.app.Activity
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.view.View
import android.view.ViewGroup
import android.webkit.WebChromeClient
import android.webkit.WebSettings
import android.webkit.WebView
import android.widget.FrameLayout
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback
import androidx.activity.result.contract.ActivityResultContract
import androidx.activity.result.contract.ActivityResultContracts

/**
 * Android TV shell: WebView + Phase 2a SAF media bridge + Phase 2b Back/orientation.
 * HTML remains source of truth; Kotlin only supplies DocumentFile/URI trees + Back bridge.
 *
 * DEBUG builds also accept adb inject intents / broadcasts that feed the same
 * SafMediaBridge resolve path (NOT a SAF picker PASS).
 */
class MainActivity : ComponentActivity() {

    private lateinit var webView: WebView
    private lateinit var safBridge: SafMediaBridge
    private var debugInjectReceiver: BroadcastReceiver? = null

    /** Open multiple docs with persistable read grants (SAF). */
    private class OpenMultipleDocumentsPersistable : ActivityResultContract<Array<String>, List<Uri>>() {
        override fun createIntent(context: Context, input: Array<String>): Intent {
            return Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                addCategory(Intent.CATEGORY_OPENABLE)
                type = "*/*"
                putExtra(Intent.EXTRA_MIME_TYPES, input)
                putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
                addFlags(
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or
                        Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
                )
            }
        }

        override fun parseResult(resultCode: Int, intent: Intent?): List<Uri> {
            if (resultCode != Activity.RESULT_OK || intent == null) return emptyList()
            val clip = intent.clipData
            if (clip != null && clip.itemCount > 0) {
                return (0 until clip.itemCount).mapNotNull { clip.getItemAt(it).uri }
            }
            return listOfNotNull(intent.data)
        }
    }

    private class OpenDocumentPersistable : ActivityResultContract<Array<String>, Uri?>() {
        override fun createIntent(context: Context, input: Array<String>): Intent {
            return Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                addCategory(Intent.CATEGORY_OPENABLE)
                type = "*/*"
                putExtra(Intent.EXTRA_MIME_TYPES, input)
                addFlags(
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or
                        Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
                )
            }
        }

        override fun parseResult(resultCode: Int, intent: Intent?): Uri? {
            if (resultCode != Activity.RESULT_OK) return null
            return intent?.data
        }
    }

    private val openFilesLauncher =
        registerForActivityResult(OpenMultipleDocumentsPersistable()) { uris ->
            safBridge.onOpenFilesResult(uris)
        }

    private val openFolderLauncher =
        registerForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri: Uri? ->
            safBridge.onOpenFolderResult(uri)
        }

    private val openPlaylistLauncher =
        registerForActivityResult(OpenDocumentPersistable()) { uri: Uri? ->
            safBridge.onOpenPlaylistResult(uri)
        }

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        safBridge = SafMediaBridge(this) { webView }
        safBridge.attachLaunchers(openFilesLauncher, openFolderLauncher, openPlaylistLauncher)

        webView = WebView(this).apply {
            layoutParams = FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
            )
            setBackgroundColor(0xFF121212.toInt())
            isFocusable = true
            isFocusableInTouchMode = true
            requestFocus(View.FOCUS_DOWN)

            settings.apply {
                javaScriptEnabled = true
                domStorageEnabled = true
                databaseEnabled = true
                allowFileAccess = true
                allowContentAccess = true
                mediaPlaybackRequiresUserGesture = false
                mixedContentMode = WebSettings.MIXED_CONTENT_COMPATIBILITY_MODE
                cacheMode = WebSettings.LOAD_DEFAULT
                useWideViewPort = true
                loadWithOverviewMode = true
                builtInZoomControls = false
                displayZoomControls = false
            }

            addJavascriptInterface(safBridge, "AndroidBridge")
            webChromeClient = WebChromeClient()
        }

        setContentView(webView)

        // P2b: Back is handled in HTML first (strip → stage → landing). Only finish if JS declines.
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                val js = """
                    (function(){
                      try {
                        if (window.__sspHandleAndroidBack) return !!window.__sspHandleAndroidBack();
                      } catch (e) {}
                      return false;
                    })()
                """.trimIndent()
                webView.evaluateJavascript(js) { result ->
                    val handled = result == "true"
                    if (!handled) {
                        isEnabled = false
                        onBackPressedDispatcher.onBackPressed()
                        isEnabled = true
                    }
                }
            }
        })

        loadSyncedViewer()
        if (BuildConfig.DEBUG) {
            registerDebugInjectReceiver()
            // Delay until WebView has loaded HTML + __sspDebugInjectSeed.
            webView.postDelayed({ maybeHandleDebugInjectIntent(intent) }, 900)
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        if (BuildConfig.DEBUG) {
            maybeHandleDebugInjectIntent(intent)
        }
    }

    /**
     * DEBUG only. Triggered by:
     *   adb shell am broadcast -a com.johnalindogan.slideshowpro.tv.DEBUG_URI_INJECT \
     *     -n com.johnalindogan.slideshowpro.tv/.MainActivity \
     *     --ei ensure 220
     * or:
     *   adb shell am start -n com.johnalindogan.slideshowpro.tv/.MainActivity \
     *     -a com.johnalindogan.slideshowpro.tv.DEBUG_URI_INJECT --ei ensure 220
     *
     * Optional extras: seed (path), ensure (int), append (bool).
     * Inject greens bridge ingest only — does NOT equal SAF picker PASS.
     */
    private fun maybeHandleDebugInjectIntent(intent: Intent?) {
        if (!BuildConfig.DEBUG || intent == null) return
        val action = intent.action
        val flag = intent.getBooleanExtra(EXTRA_DEBUG_INJECT, false)
        if (action != SafMediaBridge.DEBUG_INJECT_ACTION && !flag) return
        val seed = intent.getStringExtra(EXTRA_SEED) ?: ""
        val ensure = intent.getIntExtra(EXTRA_ENSURE, SafMediaBridge.DEBUG_DEFAULT_ENSURE)
        val append = intent.getBooleanExtra(EXTRA_APPEND, false)
        Log.i(TAG, "DEBUG_URI_INJECT ensure=$ensure seed=$seed (bridge path only; NOT SAF PASS)")
        // Drive through HTML helper so androidCall pending map + openAndroidFolder path run.
        val seedJs = seed.replace("\\", "\\\\").replace("'", "\\'")
        val js = "window.__sspDebugInjectSeed && window.__sspDebugInjectSeed({" +
            "ensure:" + ensure + "," +
            "append:" + append + "," +
            "seed:'" + seedJs + "'" +
            "});"
        webView.evaluateJavascript(js, null)
        // Clear so rotate / re-deliver does not re-inject.
        intent.action = Intent.ACTION_MAIN
        intent.removeExtra(EXTRA_DEBUG_INJECT)
    }

    private fun registerDebugInjectReceiver() {
        if (debugInjectReceiver != null) return
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context?, intent: Intent?) {
                maybeHandleDebugInjectIntent(intent)
            }
        }
        debugInjectReceiver = receiver
        val filter = IntentFilter(SafMediaBridge.DEBUG_INJECT_ACTION)
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(receiver, filter, Context.RECEIVER_EXPORTED)
        } else {
            @Suppress("UnspecifiedRegisterReceiverFlag")
            registerReceiver(receiver, filter)
        }
        Log.i(TAG, "DEBUG inject broadcast registered (NOT SAF picker PASS)")
    }

    private fun loadSyncedViewer() {
        val name = "SlideShowPro.html"
        val scheme = "file"
        val hostPath = "/android_asset/"
        webView.loadUrl(scheme + "://" + hostPath + name)
    }

    override fun onResume() { super.onResume(); webView.onResume() }
    override fun onPause() { webView.onPause(); super.onPause() }
    override fun onDestroy() {
        debugInjectReceiver?.let {
            try { unregisterReceiver(it) } catch (_: Exception) {}
            debugInjectReceiver = null
        }
        webView.destroy()
        super.onDestroy()
    }

    companion object {
        private const val TAG = "MainActivity"
        const val EXTRA_DEBUG_INJECT = "ssp_debug_inject"
        const val EXTRA_SEED = "seed"
        const val EXTRA_ENSURE = "ensure"
        const val EXTRA_APPEND = "append"
    }
}
