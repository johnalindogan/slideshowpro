# SlideShowX — Android TV (Phase 2a)

Thin Leanback / Android TV shell for **SlideShowX** that loads the existing SlideShowPro.html viewer in a WebView,
plus a SAF / DocumentFile media bridge (Kotlin ↔ JS). Same idea as the Tauri wrapper: no engine rewrite.

## Package

- applicationId: com.johnalindogan.slideshowpro.tv
- Module path: android/app
- Entry: MainActivity loads synced assets/SlideShowPro.html
- Bridge: `AndroidBridge` (SafMediaBridge) — Open Folder / Files / Playlist via SAF

## Phase 2a (shipped here)

- SAF Open Folder via DocumentFile tree URI (NOT MediaStore)
- Recursive media climb with cancel token; JS batches + yields for N≥200 (design for ~1k)
- Open files + playlists via SAF (app image/video exts; `.json` / `.ssp` / `.m3u`)
- Persistable URI permission so Continue / last folder survives relaunch
- Landing → media on stage after pick+ingest

## DEBUG URI inject (AOSP TV AVD without DocumentsUI)

AOSP TV emulator images often ship **without DocumentsUI**, so `OPEN_DOCUMENT_TREE` /
`OPEN_DOCUMENT` stub out and return no URIs. Real **P2a SAF PASS** still requires a physical
Google TV (or any device with DocumentsUI).

Meanwhile, **debug APKs only** (`BuildConfig.DEBUG`) expose a URI/tree inject path that feeds
the **same** `SafMediaBridge` → `__sspAndroidResolve` → `openAndroidFolder` / `openMediaPaths`
ingest used by real SAF picks. Use it to verify:

- batched ingest N≥200
- mid-cancel via existing `cancelIngest` / folder ingest gen (when walk+ingest is in flight)
- stage shows media
- persist + kill/relaunch **Continue** restore (seed `file://` tree re-walk)

### Important

> **Inject greens the bridge path only. It does NOT equal SAF picker PASS.**

Release builds: inject methods no-op / reject; no broadcast registration; landing **QA Inject**
button stays hidden (`debugInjectAvailable()` is false).

### Seed folder

Default (created automatically under app-private external files):

```text
/sdcard/Android/data/com.johnalindogan.slideshowpro.tv/files/ssp-qa/
```

Also probed (if already present): `/sdcard/Download/ssp-qa/`, `/sdcard/ssp-qa/`.

If the seed has fewer than `ensure` media files (default **220**), the debug helper synthesizes
tiny JPEGs into the app-private seed so App QA can hit N≥200 without pushing a library.

Optional: push your own smoke media (same set as Desktop `AppQA-smoke\atv-p2a\`):

```bash
adb shell mkdir -p /sdcard/Android/data/com.johnalindogan.slideshowpro.tv/files/ssp-qa
adb push path/to/media/. /sdcard/Android/data/com.johnalindogan.slideshowpro.tv/files/ssp-qa/
```

### How App QA triggers inject on `tv_api34`

1. `assembleDebug` + sideload + launch Leanback entry.
2. Prefer one of:
   - Landing **QA Inject** button (debug APK only), or
   - adb start (works cold or warm):

```bash
adb shell am start -n com.johnalindogan.slideshowpro.tv/.MainActivity \
  --ez ssp_debug_inject true --ei ensure 220
```

   - adb broadcast (app must already be running):

```bash
adb shell am broadcast -a com.johnalindogan.slideshowpro.tv.DEBUG_URI_INJECT --ei ensure 220
```

3. Confirm stage fills (≥200 when ensure=220), then force-stop / relaunch and tap **Continue**.
4. Optional mid-cancel: while ingest is running, call `AndroidBridge.cancelIngest()` (or navigate
   away that bumps folder ingest gen).
5. Logcat tag: `SafMediaBridge` / `MainActivity` — look for `DEBUG inject` / `NOT SAF PASS`.

JS bridge (debug only): `AndroidBridge.debugInjectFromSeed(requestId, append, seedHint, ensureCount)`
and `debugInjectPayload(requestId, json)` — also exposed as `window.__sspDebugInjectSeed({ensure, seed, append})`.

## Prerequisites

- Android Studio + JDK 17
- ANDROID_HOME or android/local.properties sdk.dir
- SDK Platform 34 + TV emulator image

Open the android/ folder in Android Studio.

## Sync viewer HTML

Mirror of sync-ui for the TV module assets.
Use the package.json script that invokes scripts/sync-android-ui.mjs.
Gradle preBuild also performs the HTML asset copy.

## Build

From android/: ./gradlew :app:assembleDebug
Install APK on TV emulator; launch Leanback entry.

## Out of scope

- P2b portrait / Leanback D-pad chrome
- P2c Ken Burns (Kotlin)
- MediaStore
- Jellyfin / Play Store

Do not merge until Senior Dev GO.
