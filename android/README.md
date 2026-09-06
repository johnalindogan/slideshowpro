# SlideShow Pro — Android TV (Phase 1)

Thin Leanback / Android TV shell that loads the existing SlideShowPro.html viewer in a WebView.
Same idea as the Tauri wrapper: no engine rewrite.

## Package

- applicationId: com.johnalindogan.slideshowpro.tv
- Module path: android/app
- Entry: MainActivity loads synced assets/SlideShowPro.html

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

SAF, media bridge, Leanback chrome rewrite, Jellyfin, Play Store, Ken Burns in Kotlin.

Do not merge until Senior Dev GO.
