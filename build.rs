use std::env;
use std::process::Command;

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|output| !output.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn emit_build_metadata() {
    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "unknown".to_string());
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_else(|_| "unknown".to_string());
    let profile = env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    let opt_level = env::var("OPT_LEVEL").unwrap_or_else(|_| "unknown".to_string());
    let debug_info = env::var("DEBUG").unwrap_or_else(|_| "unknown".to_string());
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_version = command_output(&rustc, &["--version"]);
    let git_commit = command_output("git", &["rev-parse", "--short=12", "HEAD"]);
    let source_date_epoch = env::var("SOURCE_DATE_EPOCH").unwrap_or_else(|_| "unset".to_string());

    println!("cargo:rustc-env=BLOADER_BUILD_TARGET={target}");
    println!("cargo:rustc-env=BLOADER_BUILD_TARGET_ARCH={target_arch}");
    println!("cargo:rustc-env=BLOADER_BUILD_TARGET_ENV={target_env}");
    println!("cargo:rustc-env=BLOADER_BUILD_PROFILE={profile}");
    println!("cargo:rustc-env=BLOADER_BUILD_OPT_LEVEL={opt_level}");
    println!("cargo:rustc-env=BLOADER_BUILD_DEBUG_INFO={debug_info}");
    println!("cargo:rustc-env=BLOADER_RUSTC_VERSION={rustc_version}");
    println!("cargo:rustc-env=BLOADER_GIT_COMMIT={git_commit}");
    println!("cargo:rustc-env=BLOADER_SOURCE_DATE_EPOCH={source_date_epoch}");
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    emit_build_metadata();

    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = winres::WindowsResource::new();
        let pkg_version = env::var("CARGO_PKG_VERSION").unwrap();
        let version_win = format!("{}.0", pkg_version);
        res.set("FileVersion", &version_win);
        res.set("ProductVersion", &version_win);
        res.set("FileDescription", "BLoader - Minecraft Bedrock Mod Loader");
        res.set("ProductName", "BLoader");
        res.set("InternalName", "BLoader");
        res.set("OriginalFilename", "BLoader.dll");
        res.set("CompanyName", "Chlna6666");
        res.set("LegalCopyright", "Copyright (C) 2026 Chlna6666");
        res.set(
            "Comments",
            "Open source under GPL-3.0 | https://github.com/Chlna6666/BLoader",
        );
        res.compile().unwrap();
    }
}
