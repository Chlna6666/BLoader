/// Visible BLoader build and project identity.
///
/// Package metadata is sourced directly from Cargo at compile time. Build-specific
/// fields are injected by build.rs so runtime diagnostics can report exactly which
/// binary is executing without reading sidecar files.
pub const NAME: &str = "BLoader";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");
pub const LICENSE: &str = env!("CARGO_PKG_LICENSE");
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
pub const PROFILE: &str = "native-preload-mod-crash-diagnostics";

pub const BUILD_TARGET: &str = env!("BLOADER_BUILD_TARGET");
pub const BUILD_TARGET_ARCH: &str = env!("BLOADER_BUILD_TARGET_ARCH");
pub const BUILD_TARGET_ENV: &str = env!("BLOADER_BUILD_TARGET_ENV");
pub const BUILD_PROFILE: &str = env!("BLOADER_BUILD_PROFILE");
pub const BUILD_OPT_LEVEL: &str = env!("BLOADER_BUILD_OPT_LEVEL");
pub const BUILD_DEBUG_INFO: &str = env!("BLOADER_BUILD_DEBUG_INFO");
pub const RUSTC_VERSION: &str = env!("BLOADER_RUSTC_VERSION");
pub const GIT_COMMIT: &str = env!("BLOADER_GIT_COMMIT");
pub const SOURCE_DATE_EPOCH: &str = env!("BLOADER_SOURCE_DATE_EPOCH");

pub fn enabled_features() -> String {
    let mut features = Vec::new();
    if cfg!(feature = "panel-ui") {
        features.push("panel-ui");
    }
    if cfg!(feature = "mc-symbols") {
        features.push("mc-symbols");
    }
    if cfg!(feature = "blgen") {
        features.push("blgen");
    }
    if features.is_empty() {
        "default-lightweight".to_string()
    } else {
        features.join(",")
    }
}

pub fn build_mode() -> &'static str {
    if cfg!(debug_assertions) {
        "debug-assertions"
    } else {
        "release-assertions-off"
    }
}
