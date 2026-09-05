use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

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

fn parse_launch_args() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for maybe_file in std::env::args().skip(1) {
        if maybe_file.starts_with('-') {
            continue;
        }
        if let Ok(url) = url::Url::parse(&maybe_file) {
            if url.scheme() == "file" {
                if let Ok(path) = url.to_file_path() {
                    files.push(path);
                }
            }
            continue;
        }
        files.push(PathBuf::from(maybe_file));
    }
    files.into_iter().filter(|p| is_image_path(p)).collect()
}

fn store_launch_paths(app: &AppHandle, files: Vec<PathBuf>) {
    let paths: Vec<String> = files
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    if let Some(state) = app.try_state::<LaunchState>() {
        if let Ok(mut guard) = state.paths.lock() {
            *guard = paths.clone();
        }
    }

    let json = serde_json::to_string(&paths).unwrap_or_else(|_| "[]".to_string());
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.eval(&format!("window.__SSP_LAUNCH_PATHS__ = {json};"));
    }
}

#[tauri::command]
fn get_launch_paths(state: State<'_, LaunchState>) -> LaunchPathsPayload {
    let paths = state.paths.lock().map(|g| g.clone()).unwrap_or_default();
    LaunchPathsPayload { paths }
}

#[tauri::command]
fn read_media_file(path: String, state: State<'_, LaunchState>) -> Result<Vec<u8>, String> {
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
    std::fs::read(&canonical).map_err(|e| e.to_string())
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
                    if let Some(win) = app.get_webview_window("main") {
                        let _ = win.emit("ssp-launch-paths", ());
                    }
                }
            }
            let _ = (app, &event);
        });
}
