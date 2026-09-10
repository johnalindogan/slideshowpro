package com.johnalindogan.slideshowpro.tv

import android.app.Activity
import android.content.Intent
import android.content.SharedPreferences
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.util.Base64
import android.util.Log
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.result.ActivityResultLauncher
import androidx.documentfile.provider.DocumentFile
import org.json.JSONArray
import org.json.JSONObject
import java.io.BufferedReader
import java.io.InputStreamReader
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger

/**
 * Phase 2a SAF ↔ WebView bridge.
 *
 * Mirrors Tauri shapes where practical:
 * - list_folder_media → { paths, truncated, root }
 * - open files → content URI strings used as media URLs
 * - read_text_file → playlist JSON / M3U text
 *
 * DocumentFile tree walk only (no MediaStore). Batched + cancelable for N≥200 / ~1k.
 */
class SafMediaBridge(
    private val activity: Activity,
    private val webViewProvider: () -> WebView
) {
    private val mainHandler = Handler(Looper.getMainLooper())
    private val io = Executors.newSingleThreadExecutor()
    private val prefs: SharedPreferences =
        activity.getSharedPreferences(PREFS, Activity.MODE_PRIVATE)

    /** Mid-ingest cancel token (mirrors HTML folderIngestGen). */
    private val ingestGen = AtomicInteger(0)

    private var openFilesLauncher: ActivityResultLauncher<Array<String>>? = null
    private var openFolderLauncher: ActivityResultLauncher<Uri?>? = null
    private var openPlaylistLauncher: ActivityResultLauncher<Array<String>>? = null

    private var pendingRequestId: String? = null
    private var pendingAppend: Boolean = false

    fun attachLaunchers(
        openFiles: ActivityResultLauncher<Array<String>>,
        openFolder: ActivityResultLauncher<Uri?>,
        openPlaylist: ActivityResultLauncher<Array<String>>
    ) {
        openFilesLauncher = openFiles
        openFolderLauncher = openFolder
        openPlaylistLauncher = openPlaylist
    }

    @JavascriptInterface
    fun isAvailable(): Boolean = true

    @JavascriptInterface
    fun getPersistedTreeUri(): String = prefs.getString(PREF_TREE, "") ?: ""

    @JavascriptInterface
    fun cancelIngest() {
        ingestGen.incrementAndGet()
    }

    /** Current cancel generation — JS may snapshot before long walks. */
    @JavascriptInterface
    fun currentIngestGen(): Int = ingestGen.get()

    @JavascriptInterface
    fun pickOpenFiles(requestId: String, appendJson: String) {
        pendingRequestId = requestId
        pendingAppend = appendJson == "true" || appendJson == "1"
        mainHandler.post {
            try {
                openFilesLauncher?.launch(MEDIA_MIME_TYPES)
                    ?: reject(requestId, "open files launcher unavailable")
            } catch (e: Exception) {
                Log.w(TAG, "pickOpenFiles", e)
                reject(requestId, e.message ?: "pickOpenFiles failed")
            }
        }
    }

    @JavascriptInterface
    fun pickOpenFolder(requestId: String, appendJson: String) {
        pendingRequestId = requestId
        pendingAppend = appendJson == "true" || appendJson == "1"
        mainHandler.post {
            try {
                openFolderLauncher?.launch(null)
                    ?: reject(requestId, "open folder launcher unavailable")
            } catch (e: Exception) {
                Log.w(TAG, "pickOpenFolder", e)
                reject(requestId, e.message ?: "pickOpenFolder failed")
            }
        }
    }

    @JavascriptInterface
    fun pickPlaylist(requestId: String) {
        pendingRequestId = requestId
        pendingAppend = false
        mainHandler.post {
            try {
                openPlaylistLauncher?.launch(PLAYLIST_MIME_TYPES)
                    ?: reject(requestId, "playlist launcher unavailable")
            } catch (e: Exception) {
                Log.w(TAG, "pickPlaylist", e)
                reject(requestId, e.message ?: "pickPlaylist failed")
            }
        }
    }

    /** Re-walk last persisted tree URI (Continue / last folder). */
    @JavascriptInterface
    fun reopenPersistedFolder(requestId: String, appendJson: String) {
        val tree = prefs.getString(PREF_TREE, null)
        if (tree.isNullOrBlank()) {
            reject(requestId, "no persisted folder")
            return
        }
        pendingAppend = appendJson == "true" || appendJson == "1"
        walkTreeAndResolve(requestId, Uri.parse(tree), persist = false)
    }

    /**
     * Read small text files (playlist JSON / M3U / SSP) via ContentResolver.
     * Returns UTF-8 text or throws to JS as reject when used through pickPlaylist.
     */
    @JavascriptInterface
    fun readTextUri(uriString: String): String {
        val uri = Uri.parse(uriString)
        activity.contentResolver.openInputStream(uri)?.use { input ->
            return BufferedReader(InputStreamReader(input, Charsets.UTF_8)).readText()
        } ?: throw IllegalStateException("cannot open uri")
    }

    /**
     * Optional fallback: base64 of a media file (images / small videos). Prefer content:// URL in JS.
     */
    @JavascriptInterface
    fun readMediaBase64(uriString: String, maxBytes: Int): String {
        val uri = Uri.parse(uriString)
        val limit = maxBytes.coerceIn(1, 32 * 1024 * 1024)
        activity.contentResolver.openInputStream(uri)?.use { input ->
            val buf = ByteArray(limit + 1)
            var off = 0
            while (off <= limit) {
                val n = input.read(buf, off, buf.size - off)
                if (n <= 0) break
                off += n
            }
            if (off > limit) throw IllegalStateException("file too large")
            return Base64.encodeToString(buf, 0, off, Base64.NO_WRAP)
        } ?: throw IllegalStateException("cannot open uri")
    }

    fun onOpenFilesResult(uris: List<Uri>) {
        val id = pendingRequestId ?: return
        pendingRequestId = null
        if (uris.isEmpty()) {
            resolve(id, JSONObject().put("kind", "cancel"))
            return
        }
        takeReadPermissions(uris)
        val paths = JSONArray()
        uris.forEach { paths.put(mediaEntryJson(it)) }
        resolve(
            id,
            JSONObject()
                .put("kind", "android-paths")
                .put("paths", paths)
                .put("append", pendingAppend)
        )
    }

    fun onOpenFolderResult(treeUri: Uri?) {
        val id = pendingRequestId ?: return
        pendingRequestId = null
        if (treeUri == null) {
            resolve(id, JSONObject().put("kind", "cancel"))
            return
        }
        walkTreeAndResolve(id, treeUri, persist = true)
    }

    fun onOpenPlaylistResult(uri: Uri?) {
        val id = pendingRequestId ?: return
        pendingRequestId = null
        if (uri == null) {
            resolve(id, JSONObject().put("kind", "cancel"))
            return
        }
        takeReadPermissions(listOf(uri))
        io.execute {
            try {
                val name = DocumentFile.fromSingleUri(activity, uri)?.name ?: uri.lastPathSegment ?: "playlist"
                val text = activity.contentResolver.openInputStream(uri)?.use { input ->
                    BufferedReader(InputStreamReader(input, Charsets.UTF_8)).readText()
                } ?: throw IllegalStateException("cannot read playlist")
                val ext = name.substringAfterLast('.', "").lowercase()
                val payload = JSONObject()
                    .put("kind", "android-playlist")
                    .put("uri", uri.toString())
                    .put("name", name)
                    .put("ext", ext)
                    .put("text", text)
                // Parent tree hint for relative M3U entries (best-effort).
                DocumentFile.fromSingleUri(activity, uri)?.parentFile?.uri?.let {
                    payload.put("parentUri", it.toString())
                }
                resolve(id, payload)
            } catch (e: Exception) {
                Log.w(TAG, "playlist read", e)
                reject(id, e.message ?: "playlist read failed")
            }
        }
    }

    private fun walkTreeAndResolve(requestId: String, treeUri: Uri, persist: Boolean) {
        val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
        try {
            activity.contentResolver.takePersistableUriPermission(treeUri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        } catch (e: SecurityException) {
            Log.w(TAG, "takePersistableUriPermission", e)
            try {
                activity.contentResolver.takePersistableUriPermission(treeUri, flags)
            } catch (_: Exception) { /* some providers only allow read */ }
        } catch (e: Exception) {
            Log.w(TAG, "persist uri", e)
        }
        if (persist) {
            prefs.edit().putString(PREF_TREE, treeUri.toString()).apply()
        }

        val gen = ingestGen.incrementAndGet()
        io.execute {
            try {
                val root = DocumentFile.fromTreeUri(activity, treeUri)
                    ?: throw IllegalStateException("invalid tree uri")
                val paths = ArrayList<JSONObject>(256)
                var truncated = false
                val stack = ArrayDeque<DocumentFile>()
                stack.add(root)
                while (stack.isNotEmpty()) {
                    if (ingestGen.get() != gen) {
                        resolve(requestId, JSONObject().put("kind", "cancel").put("reason", "ingest-cancelled"))
                        return@execute
                    }
                    val dir = stack.removeLast()
                    val children = dir.listFiles()
                    // Yield periodically for very large dirs
                    if (paths.size > 0 && paths.size % WALK_YIELD_EVERY == 0) {
                        try { Thread.sleep(1) } catch (_: InterruptedException) {}
                    }
                    for (child in children) {
                        if (ingestGen.get() != gen) {
                            resolve(requestId, JSONObject().put("kind", "cancel").put("reason", "ingest-cancelled"))
                            return@execute
                        }
                        val name = child.name ?: continue
                        if (name.startsWith(".")) continue
                        if (child.isDirectory) {
                            stack.add(child)
                            continue
                        }
                        if (!child.isFile) continue
                        if (!isMediaDocument(child, name)) continue
                        if (paths.size >= FOLDER_MAX_FILES) {
                            truncated = true
                            break
                        }
                        paths.add(mediaEntryJson(child.uri, name, child.type))
                    }
                    if (truncated) break
                }
                if (ingestGen.get() != gen) {
                    resolve(requestId, JSONObject().put("kind", "cancel").put("reason", "ingest-cancelled"))
                    return@execute
                }
                paths.sortWith(compareBy(
                    { it.optString("name").lowercase() },
                    { it.optString("uri") }
                ))
                val arr = JSONArray()
                paths.forEach { arr.put(it) }
                resolve(
                    requestId,
                    JSONObject()
                        .put("kind", "android-folder")
                        .put("paths", arr)
                        .put("truncated", truncated)
                        .put("root", treeUri.toString())
                        .put("append", pendingAppend)
                        .put("gen", gen)
                )
            } catch (e: Exception) {
                Log.e(TAG, "walkTree", e)
                reject(requestId, e.message ?: "folder walk failed")
            }
        }
    }


    /** Prefer DocumentFile / ContentResolver MIME + display name — SAF URIs often lack extensions. */
    private fun mediaEntryJson(uri: Uri, displayName: String? = null, mimeHint: String? = null): JSONObject {
        val doc = DocumentFile.fromSingleUri(activity, uri)
        val name = when {
            !displayName.isNullOrBlank() -> displayName
            !doc?.name.isNullOrBlank() -> doc!!.name!!
            else -> uri.lastPathSegment?.substringAfterLast(':') ?: ""
        }
        val mime = when {
            !mimeHint.isNullOrBlank() -> mimeHint
            else -> activity.contentResolver.getType(uri) ?: doc?.type ?: ""
        }
        return JSONObject()
            .put("uri", uri.toString())
            .put("name", name)
            .put("mime", mime)
    }

    private fun isMediaDocument(child: DocumentFile, name: String): Boolean {
        if (isMediaName(name)) return true
        val mime = (child.type ?: "").lowercase()
        return mime.startsWith("image/") || mime.startsWith("video/")
    }

    private fun takeReadPermissions(uris: List<Uri>) {
        for (uri in uris) {
            try {
                activity.contentResolver.takePersistableUriPermission(
                    uri,
                    Intent.FLAG_GRANT_READ_URI_PERMISSION
                )
            } catch (e: Exception) {
                // Single-document grants may already be non-persistable; keep grant for session.
                Log.d(TAG, "persist file uri skipped: $uri (${e.message})")
            }
        }
    }

    private fun resolve(requestId: String, payload: JSONObject) {
        deliver(requestId, ok = true, payload.toString())
    }

    private fun reject(requestId: String, message: String) {
        deliver(requestId, ok = false, JSONObject().put("message", message).toString())
    }

    private fun deliver(requestId: String, ok: Boolean, json: String) {
        // Base64 avoids brittle JS string escaping for large playlist / path payloads.
        val b64 = Base64.encodeToString(json.toByteArray(Charsets.UTF_8), Base64.NO_WRAP)
        val safeId = jsonEscape(requestId)
        val js = "window.__sspAndroidResolve && window.__sspAndroidResolve('$safeId', ${if (ok) "true" else "false"}, atob('$b64'));"
        mainHandler.post {
            try {
                webViewProvider().evaluateJavascript(js, null)
            } catch (e: Exception) {
                Log.e(TAG, "evaluateJavascript", e)
            }
        }
    }

    companion object {
        private const val TAG = "SafMediaBridge"
        private const val PREFS = "ssp_saf"
        private const val PREF_TREE = "persisted_tree_uri"
        private const val FOLDER_MAX_FILES = 10_000
        private const val WALK_YIELD_EVERY = 64

        private val IMAGE_EXTS = setOf(
            "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "ico"
        )
        private val VIDEO_EXTS = setOf("mp4", "mov", "webm", "m4v")

        val MEDIA_MIME_TYPES = arrayOf(
            "image/*",
            "video/*",
            "image/jpeg", "image/png", "image/gif", "image/webp", "image/bmp", "image/tiff",
            "video/mp4", "video/quicktime", "video/webm"
        )
        val PLAYLIST_MIME_TYPES = arrayOf(
            "application/json",
            "application/x-mpegurl",
            "audio/x-mpegurl",
            "audio/mpegurl",
            "text/plain",
            "*/*"
        )

        fun isMediaName(name: String): Boolean {
            val ext = name.substringAfterLast('.', "").lowercase()
            return ext in IMAGE_EXTS || ext in VIDEO_EXTS
        }

        fun isPlaylistName(name: String): Boolean {
            val ext = name.substringAfterLast('.', "").lowercase()
            return ext == "json" || ext == "ssp" || ext == "m3u" || ext == "m3u8"
        }

        private fun jsonEscape(s: String): String {
            val sb = StringBuilder(s.length + 16)
            for (c in s) {
                when (c) {
                    '\\' -> sb.append("\\\\")
                    '\'' -> sb.append("\\'")
                    '\n' -> sb.append("\\n")
                    '\r' -> sb.append("\\r")
                    '\u2028' -> sb.append("\\u2028")
                    '\u2029' -> sb.append("\\u2029")
                    else -> sb.append(c)
                }
            }
            return sb.toString()
        }
    }
}
