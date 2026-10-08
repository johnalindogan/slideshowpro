fn main() {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let cargo_version = package_version(&manifest_dir.join("Cargo.toml"));
    let installer_version = json_version(&manifest_dir.join("tauri.conf.json"));
    let npm_version = json_version(&manifest_dir.join("../package.json"));

    if installer_version != cargo_version || npm_version != cargo_version {
        panic!(
            "app version drift: tauri.conf.json={installer_version} Cargo.toml={cargo_version} package.json={npm_version}"
        );
    }

    tauri_build::build()
}

/// First `"version"` string in a JSON file. For this repo that is the app version.
fn json_version(path: &std::path::Path) -> String {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("\"version\"") else {
            continue;
        };
        let rest = rest.trim().trim_start_matches(':').trim();
        let value = rest.trim_matches(|c| c == '"' || c == ',');
        if !value.is_empty() {
            return value.to_string();
        }
    }
    panic!("no version in {}", path.display());
}

/// `[package] version` from Cargo.toml, before later dependency version keys.
fn package_version(path: &std::path::Path) -> String {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut in_package = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_package {
                break;
            }
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("version") else {
            continue;
        };
        let rest = rest.trim().trim_start_matches('=').trim();
        let value = rest.trim_matches('"');
        if !value.is_empty() {
            return value.to_string();
        }
    }
    panic!("no [package] version in {}", path.display());
}
