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

/// 解析披风包扫描根（按优先级排序，去重；只读，不创建目录）。
pub fn resolve_roots(config: &Config, game_dir: &Path) -> Vec<PathBuf> {
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
    fn redirected_gdk_uses_shared_root_not_account_profile() {
        let data_root = std::env::temp_dir().join(format!(
            "bloader-shared-capes-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let shared = data_root.join("Users").join("Shared").join("games").join("com.mojang");
        let account = data_root.join("Users").join("1234567890123456789")
            .join("games").join("com.mojang");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(&account).unwrap();
        let mut config = Config::default();
        config.enable_redirection = true;
        config.redirection_root = data_root.to_string_lossy().into_owned();
        let roots = resolve_roots(&config, Path::new(r"C:\Minecraft"));
        assert!(roots.iter().any(|p| canonical_key(p) == canonical_key(&shared)));
        assert!(!roots.iter().any(|p| canonical_key(p) == canonical_key(&account)),
            "account-specific data must not be used for BMCBL Shared cosmetics: {roots:?}");
        assert!(!shared.join("capes").exists());
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
