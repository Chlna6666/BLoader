use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let cargo_toml = PathBuf::from("Cargo.toml");
    let text = fs::read_to_string(&cargo_toml).expect("read Cargo.toml");
    let value: toml::Value = toml::from_str(&text).expect("parse Cargo.toml");

    let package = value.get("package").and_then(|v| v.as_table()).expect("package table");
    let metadata = package
        .get("metadata")
        .and_then(|v| v.as_table())
        .and_then(|m| m.get("bl"))
        .and_then(|v| v.as_table());

    let package_name = env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "unknown".to_string());
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let authors = env::var("CARGO_PKG_AUTHORS")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .collect::<Vec<_>>();
    let description = env::var("CARGO_PKG_DESCRIPTION").unwrap_or_default();
    let homepage = env::var("CARGO_PKG_HOMEPAGE").unwrap_or_default();

    let mod_id = metadata
        .and_then(|m| m.get("mod_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| package_name.replace('_', "."));
    let mod_name = metadata
        .and_then(|m| m.get("mod_name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| package_name.clone());
    let api_version = metadata
        .and_then(|m| m.get("api_version"))
        .and_then(|v| v.as_integer())
        .unwrap_or(1);
    let entry = metadata
        .and_then(|m| m.get("entry"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "mod.dll".to_string());
    let requires_symbol_pack = metadata
        .and_then(|m| m.get("requires_symbol_pack"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let required_symbols = metadata
        .and_then(|m| m.get("required_symbols"))
        .and_then(|v| v.as_array())
        .map(|values| values.iter().filter_map(|value| value.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();

    println!("cargo:rustc-env=BL_MOD_ID={mod_id}");
    println!("cargo:rustc-env=BL_MOD_NAME={mod_name}");

    let json = serde_json::json!({
        "id": mod_id,
        "name": mod_name,
        "entry": entry,
        "type": "BL",
        "api_version": api_version,
        "description": description,
        "version": version,
        "authors": authors,
        "homepage": homepage,
        "requires_symbol_pack": requires_symbol_pack,
        "required_symbols": required_symbols
    });

    let manifest_text = serde_json::to_string_pretty(&json).expect("serialize manifest");
    write_manifest_outputs(&package_name, &manifest_text, &entry);

    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=assets");
}

fn write_manifest_outputs(package_name: &str, manifest_text: &str, entry: &str) {
    fs::write("manifest.json", manifest_text).expect("write manifest.json");

    if let Some(package_dir) = package_output_dir(package_name) {
        fs::create_dir_all(&package_dir).expect("create mod package dir");
        fs::write(package_dir.join("manifest.json"), manifest_text).expect("write packaged manifest.json");
        sync_assets(&package_dir);
        link_mod_binary(&package_dir, package_name, &entry);
    }
}

fn package_output_dir(package_name: &str) -> Option<PathBuf> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").ok()?);
    let profile_dir = out_dir.ancestors().nth(3)?.to_path_buf();
    Some(profile_dir.join(package_name))
}

fn sync_assets(package_dir: &PathBuf) {
    let assets_dir = PathBuf::from("assets");
    if !assets_dir.exists() {
        return;
    }
    let target_assets = package_dir.join("assets");
    let _ = fs::remove_dir_all(&target_assets);
    copy_dir_recursive(&assets_dir, &target_assets);
}

fn copy_dir_recursive(source: &PathBuf, target: &PathBuf) {
    fs::create_dir_all(target).expect("create assets dir");
    let entries = fs::read_dir(source).expect("read assets dir");
    for entry in entries {
        let entry = entry.expect("asset entry");
        let path = entry.path();
        let to = target.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &to);
        } else {
            fs::copy(&path, &to).expect("copy asset");
        }
    }
}

fn link_mod_binary(package_dir: &PathBuf, package_name: &str, entry: &str) {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let profile_dir = out_dir.ancestors().nth(3).expect("profile dir");
    let source_dll = profile_dir.join(format!("{package_name}.dll"));
    let target_dll = package_dir.join(entry);
    let _ = fs::remove_file(&target_dll);

    #[cfg(windows)]
    {
        use std::os::windows::fs::symlink_file;
        if symlink_file(&source_dll, &target_dll).is_ok() {
            return;
        }
    }

    if source_dll.exists() {
        fs::copy(source_dll, target_dll).expect("copy mod dll");
    }
}
