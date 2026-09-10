# SlideShow Pro — Android TV (Phase 2a)

Thin Leanback / Android TV shell that loads the existing SlideShowPro.html viewer in a WebView,
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
