use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::Serialize;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "ico",
];
const VIDEO_EXTS: &[&str] = &["mp4", "mov", "webm", "m4v"];

/// Full recursive folder walk with sane media-extension filter.
/// Cap guards against pathological trees (network mounts, etc.).
const FOLDER_MAX_FILES: usize = 10_000;

#[derive(Default)]
struct LaunchState {
    /// Exact paths from Open-with / CLI (also mirrored into AllowedMedia).
    paths: Mutex<Vec<String>>,
}

#[derive(Default)]
struct AllowedMedia {
    /// Canonical absolute file paths the frontend may read.
    files: Mutex<HashSet<PathBuf>>,
    /// Canonical directory roots; any media under these may be read.
    roots: Mutex<Vec<PathBuf>>,
}

#[derive(Serialize)]
struct LaunchPathsPayload {
    paths: Vec<String>,
}

#[derive(Serialize)]
struct FolderMediaPayload {
    paths: Vec<String>,
    truncated: bool,
    root: String,
}

fn ext_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

#[allow(dead_code)]
fn is_image_path(path: &Path) -> bool {
    ext_lower(path)
        .map(|e| IMAGE_EXTS.iter().any(|x| x == &e))
        .unwrap_or(false)
}

fn is_media_path(path: &Path) -> bool {
    ext_lower(path)
        .map(|e| {
            IMAGE_EXTS.iter().any(|x| x == &e) || VIDEO_EXTS.iter().any(|x| x == &e)
        })
        .unwrap_or(false)
}

fn strip_surrounding_quotes(s: &str) -> &str {
    let t = s.trim();
    if t.len() >= 2 {
        let bytes = t.as_bytes();
        if (bytes[0] == b'"' && bytes[t.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[t.len() - 1] == b'\'')
        {
            return &t[1..t.len() - 1];
        }
    }
    t
}

/// Parse CLI / Open-with args into image/video paths.
/// Only treat args that start with `file:` as URLs — `Url::parse` would otherwise
/// treat Windows paths like `C:/foo.jpg` as scheme `"c"` and drop them.
fn parse_launch_args() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for maybe_file in std::env::args().skip(1) {
        let arg = strip_surrounding_quotes(&maybe_file);
        if arg.is_empty() || arg.starts_with('-') {
            continue;
        }
        if arg.to_ascii_lowercase().starts_with("file:") {
            if let Ok(url) = url::Url::parse(arg) {
                if url.scheme() == "file" {
                    if let Ok(path) = url.to_file_path() {
                        files.push(path);
                    }
                }
            }
            continue;
        }
        files.push(PathBuf::from(arg));
    }
    files.into_iter().filter(|p| is_media_path(p)).collect()
}

fn canonicalize_path(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|e| format!("canonicalize failed for {}: {e}", path.display()))
}

fn path_under_root(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

fn is_path_allowed(canonical: &Path, allowed: &AllowedMedia) -> bool {
    if let Ok(files) = allowed.files.lock() {
        if files.contains(canonical) {
            return true;
        }
    }
    if let Ok(roots) = allowed.roots.lock() {
        if roots.iter().any(|r| path_under_root(canonical, r)) {
            return true;
        }
    }
    false
}

fn allow_exact_paths(allowed: &AllowedMedia, paths: &[PathBuf]) {
    if let Ok(mut files) = allowed.files.lock() {
        for p in paths {
            let c = p.canonicalize().unwrap_or_else(|_| p.clone());
            files.insert(c);
        }
    }
}

fn allow_root(allowed: &AllowedMedia, root: PathBuf) {
    if let Ok(mut roots) = allowed.roots.lock() {
        let c = root.canonicalize().unwrap_or(root);
        if !roots.iter().any(|r| r == &c) {
            roots.push(c);
        }
    }
}

fn inject_launch_paths_to_webview(app: &AppHandle) {
    let paths = app
        .try_state::<LaunchState>()
        .and_then(|state| state.paths.lock().ok().map(|g| g.clone()))
        .unwrap_or_default();
    if paths.is_empty() {
        return;
    }
    let json = serde_json::to_string(&paths).unwrap_or_else(|_| "[]".to_string());
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.eval(&format!("window.__SSP_LAUNCH_PATHS__ = {json};"));
        let _ = win.emit("ssp-launch-paths", ());
    }
}

fn store_launch_paths(app: &AppHandle, files: Vec<PathBuf>) {
    let paths: Vec<String> = files
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()).to_string_lossy().into_owned())
        .collect();

    if let Some(state) = app.try_state::<LaunchState>() {
        if let Ok(mut guard) = state.paths.lock() {
            *guard = paths.clone();
        }
    }
    if let Some(allowed) = app.try_state::<AllowedMedia>() {
        allow_exact_paths(&allowed, &files);
    }

    let json = serde_json::to_string(&paths).unwrap_or_else(|_| "[]".to_string());
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.eval(&format!("window.__SSP_LAUNCH_PATHS__ = {json};"));
        // Notify the HTML bridge on all platforms (Emitter was previously mac/ios-only).
        let _ = win.emit("ssp-launch-paths", ());
    }
}

#[tauri::command]
fn get_launch_paths(state: State<'_, LaunchState>) -> LaunchPathsPayload {
    let paths = state.paths.lock().map(|g| g.clone()).unwrap_or_default();
    LaunchPathsPayload { paths }
}

#[tauri::command]
fn register_allowed_paths(
    paths: Vec<String>,
    allowed: State<'_, AllowedMedia>,
) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut bufs = Vec::new();
    for p in paths {
        let pb = PathBuf::from(&p);
        let c = canonicalize_path(&pb).unwrap_or(pb);
        out.push(c.to_string_lossy().into_owned());
        bufs.push(c);
    }
    allow_exact_paths(&allowed, &bufs);
    Ok(out)
}

/// Recursively list image/video files under a user-picked folder (sane ext filter).
/// Registers the folder as an allowed root so `read_media_file` may load them.
#[tauri::command]
fn list_folder_media(
    path: String,
    allowed: State<'_, AllowedMedia>,
) -> Result<FolderMediaPayload, String> {
    let root = canonicalize_path(Path::new(&path))?;
    if !root.is_dir() {
        return Err("not a directory".into());
    }
    allow_root(&allowed, root.clone());

    let mut found: Vec<PathBuf> = Vec::new();
    let mut truncated = false;
    let mut stack = vec![root.clone()];

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') {
                continue;
            }
            let p = entry.path();
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() {
                stack.push(p);
                continue;
            }
            if !ft.is_file() || !is_media_path(&p) {
                continue;
            }
            if found.len() >= FOLDER_MAX_FILES {
                truncated = true;
                break;
            }
            found.push(p);
        }
        if truncated {
            break;
        }
    }

    found.sort();
    let paths: Vec<String> = found
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    Ok(FolderMediaPayload {
        paths,
        truncated,
        root: root.to_string_lossy().into_owned(),
    })
}

/// Soft cap for base64 blob ingest — huge videos fall back to convertFileSrc in JS.
const BLOB_INGEST_MAX_BYTES: u64 = 80 * 1024 * 1024;

#[tauri::command]
fn media_file_size(path: String, allowed: State<'_, AllowedMedia>) -> Result<u64, String> {
    let canonical = canonicalize_path(Path::new(&path))?;
    if !is_path_allowed(&canonical, &allowed) {
        return Err("path not in allowed set".into());
    }
    let meta = std::fs::metadata(&canonical).map_err(|e| e.to_string())?;
    Ok(meta.len())
}

#[tauri::command]
fn read_media_file(path: String, allowed: State<'_, AllowedMedia>) -> Result<String, String> {
    let canonical = canonicalize_path(Path::new(&path))?;
    if !is_path_allowed(&canonical, &allowed) {
        return Err("path not in allowed set".into());
    }
    if !is_media_path(&canonical) {
        return Err("unsupported extension".into());
    }
    let meta = std::fs::metadata(&canonical).map_err(|e| e.to_string())?;
    if meta.len() > BLOB_INGEST_MAX_BYTES {
        return Err("file too large for blob ingest".into());
    }
    let bytes = std::fs::read(&canonical).map_err(|e| e.to_string())?;
    Ok(B64.encode(bytes))
}

#[tauri::command]
fn read_text_file(path: String, allowed: State<'_, AllowedMedia>) -> Result<String, String> {
    let canonical = canonicalize_path(Path::new(&path))?;
    if !is_path_allowed(&canonical, &allowed) {
        return Err("path not in allowed set".into());
    }
    let meta = std::fs::metadata(&canonical).map_err(|e| e.to_string())?;
    if meta.len() > 8 * 1024 * 1024 {
        return Err("text file too large".into());
    }
    std::fs::read_to_string(&canonical).map_err(|e| e.to_string())
}


#[tauri::command]
fn open_playlist_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("playlist") {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(
        &app,
        "playlist",
        WebviewUrl::App("index.html?sspWindow=playlist".into()),
    )
    .title("SlideShowX — Media Manager")
    .inner_size(560.0, 820.0)
    .min_inner_size(360.0, 420.0)
    .resizable(true)
    .focused(true)
    .build()
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn close_playlist_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("playlist") {
        w.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn focus_playlist_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("playlist") {
        let _ = w.show();
        let _ = w.set_focus();
        Ok(())
    } else {
        Err("playlist window not open".into())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(LaunchState::default())
        .manage(AllowedMedia::default())
        .invoke_handler(tauri::generate_handler![
            get_launch_paths,
            read_media_file,
            media_file_size,
            register_allowed_paths,
            list_folder_media,
            read_text_file,
            open_playlist_window,
            close_playlist_window,
            focus_playlist_window
        ])
        .setup(|app| {
            #[cfg(any(windows, target_os = "linux"))]
            {
                let files = parse_launch_args();
                if !files.is_empty() {
                    store_launch_paths(app.handle(), files);
                }
            }
            Ok(())
        })
        // Re-inject after the page finishes loading — setup/eval can race the webview.
        .on_page_load(|webview, payload| {
            if payload.event() != PageLoadEvent::Finished {
                return;
            }
            if webview.label() != "main" {
                return;
            }
            inject_launch_paths_to_webview(webview.app_handle());
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            if let tauri::RunEvent::Opened { urls } = &event {
                let files: Vec<PathBuf> = urls
                    .iter()
                    .filter_map(|u| u.to_file_path().ok())
                    .filter(|p| is_media_path(p))
                    .collect();
                if !files.is_empty() {
                    store_launch_paths(app, files);
                }
            }
            let _ = (app, &event);
        });
}
