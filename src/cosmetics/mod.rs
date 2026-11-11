//! BLoader 内置披风模块（Cosmetics 移植）。
//!
//! 来源：https://github.com/Chlna6666/Cosmetics（同作者、GPL-3.0-only）。
//! 通过 WinHTTP 边界内的响应覆盖向更衣室披风页追加本地披风条目、
//! 为本地商品合成 PlayFab 目录响应，并从内存下发虚拟 persona ZIP。
//! 不拦截 TLS、不修改账号数据、不触碰官方 premium_cache。
//!
//! 与独立 DLL 的差异：
//! - 内置于 BLoader.dll，bootstrap 阶段（OEP 前）安装钩子，
//!   覆盖启动期 marketplace 请求（独立 DLL 的迟到注入会漏掉）；
//! - 内嵌 Bedrock-Cosmos 元数据预设与内嵌默认 cosmetics.json 全部移除，
//!   规则完全来自外部 `cosmetics.json`（缺失时使用本地化默认规则，
//!   不含任何 Cosmos 条目）；
//! - 披风包除原 ZIP 外支持解包文件夹，并可扫描游戏 com.mojang 的
//!   `capes/` 目录（GDK/UWP/预览/重定向感知，见 `cape_roots`）；
//! - 高像素披风纹理固定规范到 64x32，避免玩家披风不显示、鞘翅正常。
pub mod cape_roots;
pub mod cape_sections;
pub mod codec;
pub mod debug;
pub mod diagnostics;
pub mod local_capes;
pub mod persona;
pub mod persona_pack;
pub mod response;
mod winhttp;

/// Native WinHTTP callbacks can fault in libHttpClient.dll while BLoader is
/// still on the stack. The crash handler uses this context for attribution.
pub(crate) fn inside_game_callback() -> bool {
    winhttp::inside_game_callback()
}

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::Config;
use crate::platform::logging;

static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
static INSTALL_ATTEMPTED: AtomicBool = AtomicBool::new(false);


/// Built-in HTTP rules are immutable and local-only. No standalone
/// Cosmetics config or overlay files are read during game startup.
const DEFAULT_RULES_JSON: &str = r#"{
  "enabled": true,
  "max_capture_bytes": 4194304,
  "debug": {"verbose": true, "response_shape": false},
  "rules": [
    {
      "name": "dressing_room_capes",
      "host": "store.mktpl.minecraft-services.net",
      "path": "/api/v2.0/layout/pages/DressingRoom_Capes",
      "method": "GET,POST",
      "mode": "append_array",
      "file": "overrides/capes_items.json",
      "local_only": true
    },
    {
      "name": "local_persona_profile",
      "host": "store.mktpl.minecraft-services.net",
      "path": "/api/v2.0/layout/pages/DressingRoom_PersonaProfile",
      "method": "GET,POST",
      "mode": "persona_profile",
      "file": "overrides/persona_profile_items.json",
      "local_only": true
    },
    {
      "name": "catalog_custom_item_metadata",
      "host": "20ca2.playfabapi.com",
      "path": "/Catalog/GetPublishedItem",
      "method": "POST",
      "mode": "catalog_item",
      "catalog_directory": "overrides/catalog",
      "local_only": true
    }
  ]
}"#;

fn default_rules() -> response::Config {
    serde_json::from_str(DEFAULT_RULES_JSON).expect("built-in cosmetics rules JSON is valid")
}

/// bootstrap 装配入口（在 network_hook::install 之后、OEP 之前调用）。
pub fn install(config: &Config, game_dir: &Path) -> bool {
    INSTALL_ATTEMPTED.store(true, Ordering::SeqCst);
    winhttp::set_master_enabled(config.enable_cosmetics);
    if !config.enable_cosmetics {
        logging::scoped_info_message(
            "cosmetics",
            "Built-in cosmetics disabled by config; WinHTTP hooks not installed.",
        );
        return true;
    }

    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return true;
    }

    let cfg = default_rules();
    // In local-only mode the HTTP overlay never reads this path. Keep it
    // within BLoader's own namespace, never under mods/Cosmetics.
    let root = game_dir.join("BLoader");
    let roots = cape_roots::resolve_roots(config, game_dir);
    if let Err(error) = winhttp::set_runtime(
        cfg,
        root,
        roots,
        config.default_locale.clone(),
    ) {
        logging::scoped_error_message(
            "cosmetics",
            &format!("runtime init failed; game remains unmodified: {error}"),
        );
        HOOK_INSTALLED.store(false, Ordering::SeqCst);
        return false;
    }

    // Prepare the immutable local cape catalog before exposing any network
    // hook. The wardrobe never waits for a ZIP scan or image decoding from a
    // WinHTTP callback, and every synthetic response reads only cached bytes.
    // Missing thumbnails can be generated separately in a future cache job.
    winhttp::prepare_local_resources();

    if let Err(error) = unsafe { winhttp::install() } {
        logging::scoped_error_message(
            "cosmetics",
            &format!("hook install failed; game remains unmodified: {error}"),
        );
        HOOK_INSTALLED.store(false, Ordering::SeqCst);
        return false;
    }
    logging::scoped_info_message(
        "cosmetics",
        "local cape resources ready; WinHTTP hooks installed before OEP; config_source=builtin",
    );
    true
}

/// config.json 热重载扇出点（Config::apply_update 调用）。
pub fn update_config(config: &Config) {
    winhttp::set_master_enabled(config.enable_cosmetics);
    // 仅在启动装配已尝试过后才提示；bootstrap 里 apply_update 先于 install
    // 执行，此前的状态变化不应触发"需要重启"提示。
    if INSTALL_ATTEMPTED.load(Ordering::SeqCst)
        && !HOOK_INSTALLED.load(Ordering::SeqCst)
        && config.enable_cosmetics
    {
        logging::scoped_info_message(
            "cosmetics",
            "enable_cosmetics turned on after startup; restart the game to install WinHTTP hooks.",
        );
    }
}

/// 早期（DllMain 预加载阶段，先于 Config::load）判断内置披风是否启用。
/// 直接读 config.json，不触发 schema 重写等副作用。
fn builtin_enabled_early() -> bool {
    // The startup policy uses Config::load() before the preload phase.
    // Use identical defaults here even if the config is absent or malformed:
    // otherwise an independent Cosmetics.dll can load before the embedded hook.
    let default = crate::config::Config::default().enable_cosmetics;
    let path = crate::config::config_file_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return default;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return default;
    };
    value
        .get("enable_cosmetics")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(default)
}

/// 独立 Cosmetics.dll 抑制判断：内置启用时跳过同名原生 mod 包，
/// 避免两套 WinHTTP 钩子叠加。内置模块不会复用第三方 Mod 数据目录。
pub fn should_suppress_native_mod(package_name: &str) -> bool {
    // DllMain 预加载阶段每个包都会查询一次；结果缓存（重启生效）。
    static BUILTIN_ENABLED_EARLY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let enabled = *BUILTIN_ENABLED_EARLY.get_or_init(builtin_enabled_early);
    enabled && matches_suppressed_name(package_name)
}

fn matches_suppressed_name(package_name: &str) -> bool {
    package_name.eq_ignore_ascii_case("Cosmetics")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_local_cape_navigation_is_enabled() {
        let rules = default_rules();
        let profile = rules.rules.iter().find(|rule| {
            rule.path == "/api/v2.0/layout/pages/DressingRoom_PersonaProfile"
        }).expect("PersonaProfile local navigation rule");
        assert!(profile.local_only);
        assert!(profile.enabled);
        assert!(matches!(profile.mode, response::Mode::PersonaProfile));
    }

    #[test]
    fn default_rules_are_local_only_without_cosmos_entries() {
        let rules = default_rules();
        assert!(rules.enabled);
        assert_eq!(rules.rules.len(), 3);
        for rule in &rules.rules {
            assert!(rule.local_only, "{} must be local_only", rule.name);
        }
    }

    #[test]
    fn suppression_matches_only_the_cosmetics_package_name() {
        assert!(matches_suppressed_name("Cosmetics"));
        assert!(matches_suppressed_name("cosmetics"));
        assert!(!matches_suppressed_name("JadeLike"));
        assert!(!matches_suppressed_name(""));
    }
}
