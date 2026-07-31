use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {

        let mut res = winres::WindowsResource::new();
        let pkg_version = env::var("CARGO_PKG_VERSION").unwrap();
        let version_win = format!("{}.0", pkg_version);
        res.set("FileVersion", &version_win);
        res.set("ProductVersion", &version_win);
        res.set("FileDescription", "Minecraft Mod Preloader");
        res.set("ProductName", "Preloader");
        res.set("LegalCopyright", "Copyright (C) 2026");
        res.compile().unwrap();
    }
}
