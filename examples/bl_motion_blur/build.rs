use std::fs;
use std::path::Path;

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let manifest_path = Path::new(&out_dir).join("manifest.json");

    let manifest = serde_json::json!({
        "mod_id": "demo.motion_blur",
        "mod_name": "Motion Blur",
        "api_version": 1,
        "entry": "bl_motion_blur.dll"
    });

    fs::write(&manifest_path, manifest.to_string()).unwrap();

    println!("cargo:rerun-if-changed=build.rs");
}
