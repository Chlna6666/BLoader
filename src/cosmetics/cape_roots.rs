//! 披风包扫描根解析。
//!
//! 披风包存放在游戏 com.mojang 数据目录的 `capes/` 下（解包文件夹官方风格，
//! 无需打包；ZIP 亦受支持）。独立 DLL 时代的 `mods/Cosmetics/capes` 遗留
//! 路径不再参与扫描——内置披风是加载器功能，数据只来自游戏数据目录。
//!
//! com.mojang 位置的候选覆盖 GDK（`%APPDATA%\Minecraft Bedrock( Preview)`、
//! `C:\Users\Shared\games\com.mojang`）与 UWP 各包（零售/预览/教育版），
//! 并且在开启文件重定向时跟随重定向位置（规则引擎做词法映射）。
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::game::file_redirection;

/// UWP 包名候选（含零售、预览与教育版变体）。
const UWP_PACKAGES: &[&str] = &[
    "Microsoft.MinecraftUWP_8wekyb3d8bbwe",
    "Microsoft.MinecraftWindowsBeta_8wekyb3d8bbwe",
    "Microsoft.MinecraftEducationEdition_8wekyb3d8bbwe",
    "Microsoft.MinecraftEducationEditionBeta_8wekyb3d8bbwe",
    "Microsoft.MinecraftEducationPreview_8wekyb3d8bbwe",
];

fn canonical_key(path: &Path) -> String {
    path.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

/// com.mojang 数据目录的原始候选（游戏自身的数据根，重定向前视角）。
fn com_mojang_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        let appdata = PathBuf::from(appdata);
        candidates.push(appdata.join("Minecraft Bedrock\\Users\\Shared\\games\\com.mojang"));
        candidates
            .push(appdata.join("Minecraft Bedrock Preview\\Users\\Shared\\games\\com.mojang"));
    }
    // GDK 独立数据根（非便携、未重定向时的官方路径）。
    candidates.push(PathBuf::from(r"C:\Users\Shared\games\com.mojang"));
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let packages = PathBuf::from(local).join("Packages");
        for package in UWP_PACKAGES {
            candidates.push(
                packages
                    .join(package)
                    .join("LocalState")
                    .join("games")
                    .join("com.mojang"),
            );
        }
    }
    candidates
}

/// Find per-account GDK com.mojang roots without recursive filesystem scans.
/// Each Users/<profile>/games/com.mojang is a distinct local resource root.
fn profile_roots(data_root: &Path) -> Vec<PathBuf> {
    let Ok(users) = std::fs::read_dir(data_root.join("Users")) else {
        return Vec::new();
    };
    let mut candidates = users
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|ty| ty.is_dir()))
        .map(|entry| entry.path().join("games").join("com.mojang"))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.truncate(128);
    candidates
}

/// 解析披风包扫描根（按优先级排序，去重；不创建目标目录）。
pub fn resolve_roots(config: &Config, game_dir: &Path) -> Vec<PathBuf> {
    // Keep the data root anchored to this launcher instance.
    let mut roots = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |path: PathBuf, seen: &mut std::collections::HashSet<String>| {
        let key = canonical_key(&path);
        if seen.insert(key) {
            roots.push(path);
        }
    };

    // 1. 开启重定向时：重定向根下的两种 com.mojang 布局
    //    （GDK 镜像 Users\Shared\games\com.mojang 与 UWP 镜像 games\com.mojang）。
    if config.enable_redirection {
        if let Some(root) = file_redirection::redirection_root_path(config, game_dir) {
            push(root.join("Users\\Shared\\games\\com.mojang"), &mut seen);
            for profile in profile_roots(&root) {
                push(profile, &mut seen);
            }
            push(root.join("games\\com.mojang"), &mut seen);
        }
    }

    // 2. 原始候选（经重定向规则映射；未启用/未命中时原样返回）。
    for candidate in com_mojang_candidates() {
        let target = file_redirection::preview_redirect_target(&candidate);
        // 只接受已存在的 com.mojang，避免在全新机器上凭空构造数据目录。
        if target.is_dir() {
            push(target, &mut seen);
        }
    }
    // GDK keeps account-specific data under Users/<XUID>, not just Shared.
    // Resolve these bases through the active redirection engine to avoid
    // mixing the live game profile with another launcher's instance data.
    if let Ok(appdata) = std::env::var("APPDATA") {
        for name in ["Minecraft Bedrock", "Minecraft Bedrock Preview"] {
            let base = file_redirection::preview_redirect_target(&PathBuf::from(&appdata).join(name));
            for profile in profile_roots(&base) {
                push(profile, &mut seen);
            }
        }
    }

    // Registry::scan 会在每个根下再进入 `capes/` 子目录（与上游一致）。
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_game_data_roots_are_used() {
        // 遗留的 mods/Cosmetics/capes 不再作为扫描根；只有已存在的
        // com.mojang 数据目录（经重定向映射）参与。
        let game_dir = std::env::temp_dir().join("cosmetics_roots_game");
        let mut config = Config::default();
        config.enable_redirection = false;
        let roots = resolve_roots(&config, &game_dir);
        for root in &roots {
            let text = root.to_string_lossy().to_ascii_lowercase();
            assert!(text.contains("com.mojang"), "{:?}", roots);
            assert!(!text.contains("mods"), "{:?}", roots);
        }
    }

    #[test]
    fn discovers_account_specific_gdk_cape_roots() {
        let base = std::env::temp_dir().join(format!(
            "bloader-cape-profile-roots-{}", std::process::id()
        ));
        let path = base.join("Users").join("1234567890123456789").join("games").join("com.mojang");
        std::fs::create_dir_all(&path).unwrap();
        let roots = profile_roots(&base);
        assert!(roots.contains(&path));
        assert!(!roots.iter().any(|p| p.ends_with("capes")));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn redirected_instance_includes_account_profile_capes() {
        let data_root = std::env::temp_dir().join(format!(
            "bloader-cape-instance-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let account = data_root.join("Users").join("1234567890123456789")
            .join("games").join("com.mojang");
        std::fs::create_dir_all(&account).unwrap();
        let mut config = Config::default();
        config.enable_redirection = true;
        config.redirection_root = data_root.to_string_lossy().into_owned();
        let roots = resolve_roots(&config, Path::new(r"C:\Minecraft"));
        assert!(roots.iter().any(|root| canonical_key(root) == canonical_key(&account)),
            "redirected instance account profile must be scanned: {roots:?}");
        assert!(!account.join("capes").exists(), "root resolution must not create a cape directory");
        std::fs::remove_dir_all(&data_root).unwrap();
    }

    #[test]
    fn uwp_candidates_cover_preview_and_education() {
        let candidates = com_mojang_candidates();
        assert!(candidates.iter().any(|p| {
            p.to_string_lossy()
                .contains("Microsoft.MinecraftUWP_8wekyb3d8bbwe")
        }));
        assert!(candidates.iter().any(|p| {
            p.to_string_lossy()
                .contains("Microsoft.MinecraftWindowsBeta_8wekyb3d8bbwe")
        }));
        assert!(
            candidates
                .iter()
                .any(|p| p.to_string_lossy().starts_with("C:\\Users\\Shared"))
        );
    }
}
