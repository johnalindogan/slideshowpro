plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.johnalindogan.slideshowpro.tv"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.johnalindogan.slideshowpro.tv"
        minSdk = 21
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0-tv-phase1"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
        debug { isDebuggable = true }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
    buildFeatures { buildConfig = true }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.leanback:leanback:1.0.0")
    implementation("androidx.webkit:webkit:1.11.0")
    implementation("androidx.activity:activity-ktx:1.9.1")
}

// Mirror Tauri sync-ui: copy root SlideShowPro.html into assets before every build.
val repoRoot = rootProject.projectDir.parentFile
val htmlSource = repoRoot.resolve("SlideShowPro.html")
val assetsDir = layout.projectDirectory.dir("src/main/assets")
val htmlAsset = assetsDir.file("SlideShowPro.html")

tasks.register("syncSlideShowProHtml") {
    group = "slideshowpro"
    description = "Sync HTML viewer into assets"
    doLast {
        val dest = file("src/main/assets")
        dest.mkdirs()
        ant.invokeMethod("copy", mapOf(
            "file" to htmlSource,
            "todir" to dest
        ))
        logger.lifecycle("Synced viewer HTML into assets")
    }
}

tasks.named("preBuild") { dependsOn("syncSlideShowProHtml") }
