use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::Serialize;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, State};

const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "ico",
];

#[derive(Default)]
struct LaunchState {
    paths: Mutex<Vec<String>>,
}

#[derive(Serialize)]
struct LaunchPathsPayload {
    paths: Vec<String>,
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
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

/// Parse CLI / Open-with args into image paths.
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
    files.into_iter().filter(|p| is_image_path(p)).collect()
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
        .into_iter()
        .map(|p| p.canonicalize().unwrap_or(p).to_string_lossy().into_owned())
        .collect();

    if let Some(state) = app.try_state::<LaunchState>() {
        if let Ok(mut guard) = state.paths.lock() {
            *guard = paths.clone();
        }
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
fn read_media_file(path: String, state: State<'_, LaunchState>) -> Result<String, String> {
    let allowed = state
        .paths
        .lock()
        .map_err(|_| "launch state lock poisoned".to_string())?;
    let canonical = PathBuf::from(&path)
        .canonicalize()
        .map_err(|e| format!("canonicalize failed: {e}"))?;
    let ok = allowed.iter().any(|p| {
        PathBuf::from(p)
            .canonicalize()
            .map(|c| c == canonical)
            .unwrap_or(false)
    });
    if !ok {
        return Err("path not in launch set".into());
    }
    if !is_image_path(&canonical) {
        return Err("unsupported extension".into());
    }
    let bytes = std::fs::read(&canonical).map_err(|e| e.to_string())?;
    Ok(B64.encode(bytes))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(LaunchState::default())
        .invoke_handler(tauri::generate_handler![get_launch_paths, read_media_file])
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
                    .filter(|p| is_image_path(p))
                    .collect();
                if !files.is_empty() {
                    store_launch_paths(app, files);
                }
            }
            let _ = (app, &event);
        });
}
