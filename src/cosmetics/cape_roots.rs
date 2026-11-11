//! 披风包扫描根解析。
//!
//! 披风包存放在游戏 com.mojang 数据目录的 `capes/` 下（解包文件夹官方风格，
//! 无需打包；ZIP 亦受支持）。独立 DLL 时代的 `mods/Cosmetics/capes` 遗留
//! 路径不再参与扫描——内置披风是加载器功能，数据只来自游戏数据目录。
//!
//! com.mojang 位置的候选覆盖 GDK（`%APPDATA%\Minecraft Bedrock( Preview)`、
//! `C:\Users\Shared\games\com.mojang`）与 UWP 各包（零售/预览/教育版），
//! 启用 BMCBL 实例隔离时，直接读取该实例的 Shared/capes 资源。
//! 披风只在 WinHTTP 层修改 HTTP 响应；不借助文件重定向 Hook 发现资源。
use std::path::{Path, PathBuf};

use crate::config::Config;

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

/// The configured instance root is a physical data location, not a Hook
/// mapping. Do not query the file-redirection engine while loading capes.
fn instance_data_root(config: &Config, game_dir: &Path) -> PathBuf {
    let configured = config.redirection_root.trim();
    if configured.is_empty() {
        return game_dir.join("Minecraft Bedrock");
    }
    let configured = Path::new(configured);
    if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        game_dir.join(configured)
    }
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

    if config.enable_redirection {
        // BMCBL isolated installations must not import capes from the user's
        // unrelated system Minecraft profile. Resolve the physical game data
        // directory directly; HTTP interception does not depend on file hooks.
        let root = instance_data_root(config, game_dir);
        push(root.join("Users").join("Shared").join("games").join("com.mojang"), &mut seen);
        push(root.join("games").join("com.mojang"), &mut seen);
    } else {
        // Non-isolated GDK/UWP uses actual on-disk game data paths. This is
        // read-only discovery, not CreateFileW redirection or HTTP rewriting.
        for candidate in com_mojang_candidates() {
            if candidate.is_dir() {
                push(candidate, &mut seen);
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
        // 不读取 mods/Cosmetics/capes，或任何非 com.mojang 数据目录。
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
    fn isolated_cape_root_is_only_the_configured_instance_not_system_paths() {
        let mut config = Config::default();
        config.enable_redirection = true;
        config.redirection_root = "Minecraft Bedrock".to_string();
        let game_dir = Path::new(r"C:\BMCBL\versions\26.60");
        let roots = resolve_roots(&config, game_dir);
        assert_eq!(roots.len(), 2);
        assert_eq!(
            roots[0],
            game_dir.join("Minecraft Bedrock")
                .join("Users").join("Shared").join("games").join("com.mojang")
        );
        assert_eq!(
            roots[1],
            game_dir.join("Minecraft Bedrock").join("games").join("com.mojang")
        );
        assert!(!roots.iter().any(|root| {
            root.to_string_lossy().contains("AppData")
        }));
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
