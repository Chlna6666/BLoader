use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::OnceLock;
use notify::{Config as NotifyConfig, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;

use crate::runtime::foundation::{file_io_policy, logging};
use crate::utils::get_exe_directory;

#[cfg(feature = "panel-ui")]
const DEFAULT_TOGGLE_HOTKEY_VK: u32 = 0x4D;
#[cfg(feature = "panel-ui")]
const DEFAULT_RELOAD_HOTKEY_VK: u32 = 0x78;
#[cfg(feature = "panel-ui")]
const DEFAULT_OVERLAY_BLUR_STRENGTH: f32 = 1.0;
const DEFAULT_REDIRECTION_ROOT: &str = "Minecraft Bedrock";
const DEFAULT_LOG_ARCHIVE_DAYS: u32 = 7;
const REMOVED_CONFIG_KEYS: &[&str] = &[
    "xgameruntime_redirection",
    "editor_mode",
    "lock_mouse_on_launch",
    "unlock_mouse_hotkey",
    "reduce_pixels",
];

#[cfg(feature = "panel-ui")]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyConfig {
    #[serde(default = "default_hotkey_key")]
    pub key: u32,
    #[serde(default = "default_hotkey_alt")]
    pub alt: bool,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
}

#[cfg(feature = "panel-ui")]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct OverlaySettingsConfig {
    #[serde(default = "default_toggle_hotkey")]
    pub toggle_hotkey: HotkeyConfig,
    #[serde(default = "default_reload_hotkey")]
    pub reload_hotkey: HotkeyConfig,
    #[serde(default = "default_overlay_blur_strength")]
    pub blur_strength: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FileRedirectionConfig {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    #[serde(default = "default_false")]
    pub enable_debug_console: bool,

    #[serde(default = "default_false")]
    pub enable_redirection: bool,

    #[serde(default = "default_redirection_root")]
    pub redirection_root: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vanilla_skin_pack_redirect: Option<String>,

    #[serde(default)]
    pub file_redirections: Vec<FileRedirectionConfig>,

    #[serde(default)]
    pub mods: Vec<String>,

    #[serde(default = "default_false")]
    pub disable_mod_loading: bool,

    #[serde(default = "default_false")]
    pub enable_network_hooks: bool,

    #[serde(default = "default_false")]
    pub enable_p2p_redirection: bool,

    #[serde(default)]
    pub p2p_target_ip: String,

    #[serde(default = "default_network_listen_port")]
    pub network_listen_port: u16,

    #[serde(default = "default_true")]
    pub enable_mc_bug_fix: bool,

    #[serde(default = "default_net_hex_bytes")]
    pub network_log_hex_bytes: usize,

    #[serde(default = "default_false")]
    pub network_verbose: bool,

    #[serde(default = "default_ignore_ports")]
    pub network_ignore_ports: Vec<u16>,

    #[cfg(feature = "panel-ui")]
    #[serde(default = "default_false")]
    pub enable_bedrock_ui_reload_probe: bool,

    #[serde(default = "default_log_level")]
    pub log_level: String,

    #[serde(default = "default_log_archive_days")]
    pub log_archive_days: u32,

    #[serde(default = "default_locale")]
    pub default_locale: String,

    #[cfg(feature = "panel-ui")]
    #[serde(default = "default_false")]
    pub enable_dx11: bool,

    #[cfg(feature = "panel-ui")]
    #[serde(default = "default_overlay_settings")]
    pub overlay: OverlaySettingsConfig,
}

fn default_false() -> bool {
    false
}

fn default_true() -> bool {
    true
}

fn default_network_listen_port() -> u16 {
    19132
}

fn default_net_hex_bytes() -> usize {
    0
}

fn default_ignore_ports() -> Vec<u16> {
    vec![7897]
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_archive_days() -> u32 {
    DEFAULT_LOG_ARCHIVE_DAYS
}

fn default_locale() -> String {
    "zh_CN".to_string()
}

fn default_redirection_root() -> String {
    DEFAULT_REDIRECTION_ROOT.to_string()
}

#[cfg(feature = "panel-ui")]
fn default_hotkey_key() -> u32 {
    DEFAULT_TOGGLE_HOTKEY_VK
}

#[cfg(feature = "panel-ui")]
fn default_hotkey_alt() -> bool {
    true
}

#[cfg(feature = "panel-ui")]
fn default_toggle_hotkey() -> HotkeyConfig {
    HotkeyConfig {
        key: DEFAULT_TOGGLE_HOTKEY_VK,
        alt: true,
        ctrl: false,
        shift: false,
    }
}

#[cfg(feature = "panel-ui")]
fn default_reload_hotkey() -> HotkeyConfig {
    HotkeyConfig {
        key: DEFAULT_RELOAD_HOTKEY_VK,
        alt: false,
        ctrl: false,
        shift: false,
    }
}

#[cfg(feature = "panel-ui")]
fn default_overlay_blur_strength() -> f32 {
    DEFAULT_OVERLAY_BLUR_STRENGTH
}

#[cfg(feature = "panel-ui")]
fn clamp_overlay_blur_strength(value: f32) -> f32 {
    value.clamp(0.0, 2.4)
}

#[cfg(feature = "panel-ui")]
fn default_overlay_settings() -> OverlaySettingsConfig {
    OverlaySettingsConfig {
        toggle_hotkey: default_toggle_hotkey(),
        reload_hotkey: default_reload_hotkey(),
        blur_strength: default_overlay_blur_strength(),
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enable_debug_console: false,
            enable_redirection: false,
            redirection_root: default_redirection_root(),
            vanilla_skin_pack_redirect: None,
            file_redirections: Vec::new(),
            mods: Vec::new(),
            disable_mod_loading: false,
            enable_network_hooks: false,
            enable_p2p_redirection: false,
            p2p_target_ip: String::new(),
            network_listen_port: default_network_listen_port(),
            enable_mc_bug_fix: true,
            network_log_hex_bytes: 0,
            network_verbose: false,
            network_ignore_ports: default_ignore_ports(),
            #[cfg(feature = "panel-ui")]
            enable_bedrock_ui_reload_probe: false,
            log_level: default_log_level(),
            log_archive_days: default_log_archive_days(),
            default_locale: default_locale(),
            #[cfg(feature = "panel-ui")]
            enable_dx11: false,
            #[cfg(feature = "panel-ui")]
            overlay: default_overlay_settings(),
        }
    }
}

#[cfg(feature = "panel-ui")]
impl OverlaySettingsConfig {
    pub fn reset_to_defaults(&mut self) {
        *self = default_overlay_settings();
    }

    pub fn blur_strength(&self) -> f32 {
        clamp_overlay_blur_strength(self.blur_strength)
    }

    pub fn set_blur_strength(&mut self, value: f32) {
        self.blur_strength = clamp_overlay_blur_strength(value);
    }
}

static CONFIG_WATCHER: OnceLock<RwLock<Option<RecommendedWatcher>>> = OnceLock::new();

pub fn ensure_config_watcher() {
    let watcher_slot = CONFIG_WATCHER.get_or_init(|| RwLock::new(None));
    if watcher_slot.read().is_some() {
        return;
    }

    let cfg_path = config_path();
    let parent_dir = match cfg_path.parent() {
        Some(p) => p.to_path_buf(),
        None => return,
    };

    let target_file = cfg_path.clone();
    let watcher = RecommendedWatcher::new(
        move |result: Result<notify::Event, notify::Error>| {
            let Ok(event) = result else { return };
            if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                return;
            }
            if !event.paths.iter().any(|p| p == &target_file) {
                return;
            }

            let new_config = Config::load();
            Config::apply_update(&new_config);
        },
        NotifyConfig::default(),
    );

    if let Ok(mut w) = watcher {
        if w.watch(&parent_dir, RecursiveMode::NonRecursive).is_ok() {
            *watcher_slot.write() = Some(w);
            logging::info_message(&format!(
                "[config] Hot-reload file watcher active for {}",
                cfg_path.display()
            ));
        }
    }
}

fn is_config_file_outdated(content: &str, loaded_cfg: &Config) -> bool {
    let Ok(disk_val) = serde_json::from_str::<serde_json::Value>(content) else {
        return true;
    };
    let Ok(loaded_val) = serde_json::to_value(loaded_cfg) else {
        return false;
    };

    let (Some(disk_obj), Some(loaded_obj)) = (disk_val.as_object(), loaded_val.as_object()) else {
        return true;
    };

    if REMOVED_CONFIG_KEYS
        .iter()
        .any(|key| disk_obj.contains_key(*key))
    {
        return true;
    }

    for key in loaded_obj.keys() {
        if !disk_obj.contains_key(key) {
            return true;
        }
    }

    false
}

fn merged_config_json(content: Option<&str>, config: &Config) -> std::io::Result<String> {
    let mut output = content
        .and_then(|content| serde_json::from_str::<serde_json::Value>(content).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let typed = serde_json::to_value(config)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let typed = typed
        .as_object()
        .ok_or_else(|| std::io::Error::other("serialized config is not a JSON object"))?;

    for key in REMOVED_CONFIG_KEYS {
        output.remove(*key);
    }
    for (key, value) in typed {
        output.insert(key.clone(), value.clone());
    }

    serde_json::to_string_pretty(&output)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

impl Config {
    pub fn load() -> Self {
        let config_path = config_path();
        logging::info_message(&format!("[config] Loading configuration file from: {}", config_path.display()));

        if !config_path.exists() {
            let default_config = Config::default();
            if file_io_policy::writes_allowed() {
                let _ = default_config.save();
            } else {
                logging::info_message(
                    "[config] Legacy UWP read-only mode: config.json is absent; using in-memory defaults without creating a file.",
                );
            }
            return default_config;
        }

        if let Ok(content) = fs::read_to_string(&config_path) {
            if let Ok(mut cfg) = serde_json::from_str::<Config>(&content) {
                #[cfg(feature = "panel-ui")]
                {
                    cfg.overlay.blur_strength = clamp_overlay_blur_strength(cfg.overlay.blur_strength);
                }
                if is_config_file_outdated(&content, &cfg) {
                    if file_io_policy::writes_allowed() {
                        logging::info_message(&format!("[config] Discovered outdated config.json schema; synchronizing BLoader-owned fields without deleting launcher-owned fields at {}", config_path.display()));
                        let _ = cfg.save();
                    } else {
                        logging::info_message(
                            "[config] Legacy UWP read-only mode: outdated config.json is accepted without rewriting it.",
                        );
                    }
                }
                return cfg;
            } else {
                logging::warn_message(&format!("[config] Failed to parse existing config.json format at {}", config_path.display()));
            }
        }

        let default_config = Config::default();
        if file_io_policy::writes_allowed() {
            let _ = default_config.save();
        } else {
            logging::warn_message(
                "[config] Legacy UWP read-only mode: invalid config.json was not replaced; using in-memory defaults.",
            );
        }
        default_config
    }

    pub fn apply_update(config: &Config) {
        logging::set_archive_retention_days(config.log_archive_days);
        crate::core::network_hook::update_config(config);
        logging::info_message(&format!(
            "Config applied | locale={} | mods={} | redirection={} | network={} | listen={} | archive_days={}",
            config.default_locale,
            config.mods.len(),
            config.enable_redirection,
            config.enable_network_hooks,
            config.network_listen_port,
            config.log_archive_days
        ));
    }

    pub fn save(&self) -> std::io::Result<()> {
        if !file_io_policy::writes_allowed() {
            Self::apply_update(self);
            logging::warn_message(
                "[config] Legacy UWP read-only mode: config save skipped; changes apply only to the current process.",
            );
            return Ok(());
        }

        let path = config_path();
        let existing = fs::read_to_string(&path).ok();
        let json = merged_config_json(existing.as_deref(), self)?;
        let res = fs::write(path, json);
        if res.is_ok() {
            Self::apply_update(self);
        }
        res
    }

    pub fn reset_to_defaults(&mut self) {
        *self = Self::default();
        let _ = self.save();
    }
}

fn config_path() -> std::path::PathBuf {
    let loader_dir = crate::utils::get_loader_directory();
    let loader_cfg = loader_dir.join("config.json");
    if loader_cfg.exists() {
        return loader_cfg;
    }

    let exe_dir = get_exe_directory();
    let exe_cfg = exe_dir.join("config.json");
    if exe_cfg.exists() {
        return exe_cfg;
    }

    loader_cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_accepts_launcher_runtime_fields_only() {
        let config: Config = serde_json::from_str(
            r#"{
                "disable_mod_loading": true,
                "enable_redirection": true,
                "redirection_root": "C:\\Games\\Minecraft\\Minecraft Bedrock",
                "vanilla_skin_pack_redirect": "C:\\BMCBL\\skin_packs\\custom",
                "file_redirections": [
                    {
                        "source": "C:\\Games\\Minecraft\\data\\skin_packs\\vanilla",
                        "target": "C:\\BMCBL\\skin_packs\\custom",
                        "kind": "directory"
                    }
                ],
                "mods": ["mods/example.dll"]
            }"#,
        )
        .unwrap();

        assert!(config.disable_mod_loading);
        assert!(config.enable_redirection);
        assert_eq!(
            config.vanilla_skin_pack_redirect.as_deref(),
            Some(r"C:\BMCBL\skin_packs\custom")
        );
        assert_eq!(config.log_archive_days, DEFAULT_LOG_ARCHIVE_DAYS);
        #[cfg(feature = "panel-ui")]
        assert!(!config.enable_dx11);
        assert_eq!(config.file_redirections.len(), 1);
        assert_eq!(config.mods, vec!["mods/example.dll"]);
    }

    #[test]
    fn config_defaults_to_relative_redirection_root() {
        let config = Config::default();
        assert_eq!(config.redirection_root, DEFAULT_REDIRECTION_ROOT);
        assert!(config.vanilla_skin_pack_redirect.is_none());
        assert_eq!(config.log_archive_days, DEFAULT_LOG_ARCHIVE_DAYS);

        let parsed: Config = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(parsed.redirection_root, DEFAULT_REDIRECTION_ROOT);
        assert!(parsed.vanilla_skin_pack_redirect.is_none());
        assert_eq!(parsed.log_archive_days, DEFAULT_LOG_ARCHIVE_DAYS);
    }

    #[test]
    fn config_accepts_user_custom_json() {
        let json = r#"{
          "enable_debug_console": true,
          "enable_redirection": false,
          "disable_mod_loading": false,
          "vanilla_skin_pack_redirect": null,
          "log_archive_days": 14,
          "file_redirections": [],
          "redirection_root": "Minecraft Bedrock",
          "mods": []
        }"#;

        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.enable_debug_console);
        assert!(!config.enable_redirection);
        assert!(!config.disable_mod_loading);
        assert!(config.vanilla_skin_pack_redirect.is_none());
        assert_eq!(config.log_archive_days, 14);
        assert_eq!(config.redirection_root, "Minecraft Bedrock");
    }

    #[test]
    fn config_detects_outdated_file_and_prepares_sync() {
        let old_json = r#"{
          "enable_debug_console": true,
          "enable_redirection": false
        }"#;

        let loaded: Config = serde_json::from_str(old_json).unwrap();
        assert!(is_config_file_outdated(old_json, &loaded));

        let complete_json = serde_json::to_string_pretty(&loaded).unwrap();
        assert!(!is_config_file_outdated(&complete_json, &loaded));
    }

    #[test]
    fn config_sync_removes_dead_bloader_fields_and_preserves_launcher_fields() {
        let legacy_json = r#"{
          "editor_mode": false,
          "lock_mouse_on_launch": false,
          "unlock_mouse_hotkey": "ALT",
          "reduce_pixels": 20,
          "shortcut_silent_launch": true,
          "launcher_owned_value": "keep-me"
        }"#;
        let config: Config = serde_json::from_str(legacy_json).unwrap();
        assert!(is_config_file_outdated(legacy_json, &config));

        let merged = merged_config_json(Some(legacy_json), &config).unwrap();
        let value: serde_json::Value = serde_json::from_str(&merged).unwrap();
        let object = value.as_object().unwrap();

        for key in [
            "editor_mode",
            "lock_mouse_on_launch",
            "unlock_mouse_hotkey",
            "reduce_pixels",
        ] {
            assert!(!object.contains_key(key));
        }
        assert_eq!(object.get("shortcut_silent_launch"), Some(&serde_json::json!(true)));
        assert_eq!(
            object.get("launcher_owned_value"),
            Some(&serde_json::json!("keep-me"))
        );
        assert_eq!(
            object.get("log_archive_days"),
            Some(&serde_json::json!(DEFAULT_LOG_ARCHIVE_DAYS))
        );
    }
}
