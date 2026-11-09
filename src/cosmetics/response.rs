//! Conservative JSON response overlays.
//! Never replace the user's original inventory: append-only, identity deduplicated,
//! and skip on an unrecognized schema. PlayFab item overrides are opt-in by item ID.
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

fn default_limit() -> usize {
    4 * 1024 * 1024
}
fn default_method() -> String {
    "GET".into()
}
fn default_identity() -> String {
    "id".into()
}
fn default_enabled() -> bool {
    true
}
#[derive(Debug, Deserialize, Clone)]
pub struct DebugConfig {
    #[serde(default = "default_enabled")]
    pub verbose: bool,
    #[serde(default = "default_enabled")]
    pub response_shape: bool,
}
impl Default for DebugConfig {
    fn default() -> Self {
        Self {
            verbose: true,
            response_shape: true,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_limit")]
    pub max_capture_bytes: usize,
    #[serde(default)]
    pub debug: DebugConfig,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    AppendArray,
    CatalogItem,
    ReplaceJson,
    PersonaProfile,
    InventoryProbe,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Rule {
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub host: String,
    pub path: String,
    #[serde(default = "default_method")]
    pub method: String,
    pub mode: Mode,
    #[serde(default)]
    pub pointer: String,
    #[serde(default)]
    pub file: String,
    #[serde(default = "default_identity")]
    pub identity_key: String,
    #[serde(default)]
    pub catalog_directory: String,
    /// Optional local-only UI projection of official cape ownership.
    /// This never produces entitlement receipts or mutates the inventory API.
    #[serde(default)]
    pub official_preview_ids: String,
    /// Only use ZIPs in capes/; completely ignore old Cosmos overrides/catalog.
    #[serde(default)]
    pub local_only: bool,
}

impl Config {
    pub fn observe_rule(&self, host: &str, path: &str, method: &str) -> Option<Rule> {
        if !self.enabled {
            return None;
        }
        self.rules
            .iter()
            .find(|r| {
                let path_matches = r.path == path
                    || (r
                        .host
                        .eq_ignore_ascii_case("store.mktpl.minecraft-services.net")
                        && !r.path.contains('?')
                        && path.split('?').next().is_some_and(|p| p == r.path));
                r.host.eq_ignore_ascii_case(host)
                    && path_matches
                    && r.method
                        .split(',')
                        .any(|m| m.trim().eq_ignore_ascii_case(method))
            })
            .cloned()
    }
    pub fn match_rule(&self, host: &str, path: &str, method: &str) -> Option<Rule> {
        self.observe_rule(host, path, method)
            .filter(|rule| rule.enabled)
    }
}

fn guarded_path(base: &Path, child: &str) -> Result<PathBuf, String> {
    let relative = Path::new(child);
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("拒绝非相对安全资源路径".into());
    }
    Ok(base.join(relative))
}

/// Change UI-only ownership on an allowlisted Minecraft cape.
/// Does not add inventory entries, purchase receipts, signatures or claims.
fn preview_official_cape(item: &mut Value, key: &str, ids: &HashSet<String>) -> bool {
    let Some(id) = item.get(key).and_then(Value::as_str) else {
        return false;
    };
    if !ids.contains(&id.to_ascii_lowercase()) {
        return false;
    }
    if item.get("creatorName").and_then(Value::as_str) != Some("Minecraft")
        || item.get("pieceType").and_then(Value::as_str) != Some("persona_capes")
    {
        return false;
    }
    if item.get("ownership").and_then(Value::as_str) == Some("Purchased") {
        return false;
    }
    let Some(obj) = item.as_object_mut() else {
        return false;
    };
    obj.insert("ownership".to_string(), Value::from("Purchased"));
    true
}

/// Return None when no local overlay is configured or its item ID is unknown.
/// All errors leave the upstream response unchanged at the caller.
pub fn overlay(
    base: &Path,
    rule: &Rule,
    request_body: &[u8],
    original_body: &[u8],
) -> Result<Option<Vec<u8>>, String> {
    match rule.mode {
        Mode::ReplaceJson => {
            let path = guarded_path(base, &rule.file)?;
            let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            // Validate before exposing any data to the game.
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("replacement JSON invalid: {e}"))?;
            if !value.is_object() {
                return Err("replacement root must be object".into());
            }
            serde_json::to_vec(&value)
                .map(Some)
                .map_err(|e| e.to_string())
        }
        Mode::PersonaProfile => {
            let path = guarded_path(base, &rule.file)?;
            let additions: Value = serde_json::from_slice(
                &fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?,
            )
            .map_err(|e| format!("persona profile additions invalid: {e}"))?;
            let add = additions
                .as_array()
                .ok_or("persona profile items must be array")?;
            if add.is_empty() {
                return Ok(None);
            }
            let mut root: Value = serde_json::from_slice(original_body)
                .map_err(|e| format!("original PersonaProfile invalid: {e}"))?;
            let rows = root
                .pointer_mut("/result/layout/0/rows")
                .and_then(Value::as_array_mut)
                .ok_or("PersonaProfile rows array is missing")?;
            let mut inserted = 0;
            for row in rows.iter_mut() {
                if row.get("controlId").and_then(Value::as_str) != Some("StoreRow") {
                    continue;
                }
                let Some(components) = row.get_mut("components").and_then(Value::as_array_mut)
                else {
                    continue;
                };
                let persona_dropdown = components.iter().any(|c| {
                    c.get("type").and_then(Value::as_str) == Some("dropdownOptionComp")
                        && c.get("dropdownId").and_then(Value::as_i64) == Some(0)
                });
                if !persona_dropdown {
                    continue;
                }
                let Some(component) = components
                    .iter_mut()
                    .find(|c| c.get("type").and_then(Value::as_str) == Some("itemListComp"))
                else {
                    continue;
                };
                let Some(items) = component.get_mut("items").and_then(Value::as_array_mut) else {
                    continue;
                };
                let mut seen: HashSet<String> = items
                    .iter()
                    .filter_map(|v| v.get("id"))
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect();
                for item in add {
                    let id = item
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or("PersonaProfile local item has no string id")?;
                    if seen.insert(id.to_string()) {
                        // Inject the navigation item only. Never claim ownership.
                        let mut copy = item.clone();
                        if let Some(obj) = copy.as_object_mut() {
                            obj.insert("ownership".to_string(), Value::from("NotOwned"));
                        }
                        items.insert(0, copy);
                        inserted += 1;
                    }
                }
                let count = items.len();
                if let Some(obj) = component.as_object_mut() {
                    let prior_total = obj.get("totalItems").and_then(Value::as_u64).unwrap_or(0);
                    obj.insert(
                        "totalItems".into(),
                        Value::from(prior_total.max(count as u64)),
                    );
                    if let Some(config) = obj
                        .get_mut("customStoreRowConfiguration")
                        .and_then(Value::as_object_mut)
                    {
                        let prior_max =
                            config.get("maxOffers").and_then(Value::as_u64).unwrap_or(0);
                        config.insert("maxOffers".into(), Value::from(prior_max.max(count as u64)));
                    }
                }
            }
            if inserted == 0 {
                return Ok(None);
            }
            serde_json::to_vec(&root)
                .map(Some)
                .map_err(|e| e.to_string())
        }
        Mode::InventoryProbe => Ok(None),
        Mode::AppendArray => {
            let data = if rule.local_only {
                // A genuinely standalone cape folder needs no overrides/ JSON.
                b"[]".to_vec()
            } else {
                let path = guarded_path(base, &rule.file)?;
                match fs::read(&path) {
                    Ok(data) => data,
                    // BLoader 移植：移除内嵌 Cosmos 元数据回退，纯配置驱动；
                    // 文件缺失时披风页按空数组处理（不注入任何内置条目）。
                    Err(e)
                        if e.kind() == std::io::ErrorKind::NotFound
                            && rule.name == "dressing_room_capes" =>
                    {
                        b"[]".to_vec()
                    }
                    Err(e) => return Err(format!("{}: {e}", path.display())),
                }
            };
            let additions: Value =
                serde_json::from_slice(&data).map_err(|e| format!("overlay JSON: {e}"))?;
            let add = additions.as_array().ok_or("overlay must be a JSON array")?;
            // Explicit allowlist produced by the user's optional Cosmos importer.
            // Empty/missing list disables local UI ownership presentation.
            let official_preview_ids: HashSet<String> =
                if rule.local_only || rule.official_preview_ids.is_empty() {
                    HashSet::new()
                } else {
                    let file = guarded_path(base, &rule.official_preview_ids)?;
                    match fs::read(&file) {
                        Ok(raw) => {
                            let ids: Vec<String> = serde_json::from_slice(&raw)
                                .map_err(|e| format!("invalid official preview ids: {e}"))?;
                            ids.into_iter()
                                .filter(|s| is_uuid(s))
                                .map(|s| s.to_ascii_lowercase())
                                .collect()
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
                        Err(e) => return Err(format!("official preview file read failed: {e}")),
                    }
                };
            let local_items = if rule.name == "dressing_room_capes" {
                super::local_capes::items()
            } else {
                &[]
            };
            if add.is_empty() && local_items.is_empty() && official_preview_ids.is_empty() {
                return Ok(None);
            }
            let mut root: Value = serde_json::from_slice(original_body)
                .map_err(|e| format!("original JSON invalid: {e}"))?;
            if rule.name == "dressing_room_capes" {
                // Respect the original owned cape section. Cosmos itself
                // creates distinct GridList rows for Vanilla/Custom capes.
                let result = super::cape_sections::inject(
                    &mut root,
                    add,
                    local_items,
                    &official_preview_ids,
                    &rule.identity_key,
                )?;
                if !result.changed() {
                    return Ok(None);
                }
                return serde_json::to_vec(&root)
                    .map(Some)
                    .map_err(|e| e.to_string());
            }
            let target = root
                .pointer_mut(&rule.pointer)
                .and_then(Value::as_array_mut)
                .ok_or_else(|| format!("schema mismatch at {}", rule.pointer))?;
            let key = rule.identity_key.as_str();
            // Reject entries without an identity rather than silently introducing duplicates.
            let mut seen: HashSet<String> = target
                .iter()
                .filter_map(|value| value.get(key))
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect();
            // Only change the local wardrobe display ownership field for explicitly
            // selected Minecraft capes; preserve every original receipt and item field.
            let mut projected = 0usize;
            for item in target.iter_mut() {
                if preview_official_cape(item, key, &official_preview_ids) {
                    projected += 1;
                }
            }
            let mut added = 0usize;
            for item in add.iter().chain(local_items.iter()) {
                let identity = item
                    .get(key)
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("overlay item missing string identity {key}"))?;
                if seen.insert(identity.to_string()) {
                    let mut local = item.clone();
                    if preview_official_cape(&mut local, key, &official_preview_ids) {
                        projected += 1;
                    }
                    target.push(local);
                    added += 1;
                }
            }
            if added == 0 && projected == 0 {
                return Ok(None);
            }
            // Cosmos places capes in an itemListComp. A successful append must
            // update both the item count and the render limit or the new items
            // may remain invisible although JSON replacement succeeded.
            if let Some(parent_path) = rule.pointer.strip_suffix("/items") {
                if let Some(parent) = root.pointer_mut(parent_path).and_then(Value::as_object_mut) {
                    if parent.get("type").and_then(Value::as_str) == Some("itemListComp") {
                        let count = parent
                            .get("items")
                            .and_then(Value::as_array)
                            .map(|items| items.len())
                            .unwrap_or(0);
                        let prior_total = parent
                            .get("totalItems")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        parent.insert(
                            "totalItems".into(),
                            Value::from(prior_total.max(count as u64)),
                        );
                        if let Some(config) = parent
                            .get_mut("customStoreRowConfiguration")
                            .and_then(Value::as_object_mut)
                        {
                            let prior_max =
                                config.get("maxOffers").and_then(Value::as_u64).unwrap_or(0);
                            config.insert(
                                "maxOffers".into(),
                                Value::from(prior_max.max(count as u64)),
                            );
                        }
                    }
                }
            }
            serde_json::to_vec(&root)
                .map(Some)
                .map_err(|e| e.to_string())
        }
        Mode::CatalogItem => {
            let id = catalog_id(request_body)
                .map_err(|reason| format!("catalog request rejected: {reason}"))?;
            // In local-only mode reject all nonlocal product IDs without
            // even opening legacy catalog files.
            if rule.local_only {
                return match super::local_capes::catalog(&id) {
                    Some(bytes) => Ok(Some(bytes.to_vec())),
                    None => Ok(None),
                };
            }
            let folder = guarded_path(base, &rule.catalog_directory)?;
            let path = folder.join(format!("{id}.json"));
            let catalog_bytes = if let Some(bytes) = super::local_capes::catalog(&id) {
                bytes.to_vec()
            } else {
                match fs::read(&path) {
                    Ok(data) => data,
                    // BLoader 移植：移除内嵌 Cosmos 目录回退；未知条目直接透传。
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(format!("{}: {e}", path.display())),
                }
            };
            let local: Value = serde_json::from_slice(&catalog_bytes)
                .map_err(|e| format!("catalog JSON invalid: {e}"))?;
            let file_id = local
                .pointer("/data/Item/Id")
                .and_then(Value::as_str)
                .ok_or("catalog response missing data.Item.Id")?;
            if !file_id.eq_ignore_ascii_case(&id) {
                return Err("catalog response item ID does not match request".into());
            }
            super::persona::validate_local_catalog_urls(&local)?;
            serde_json::to_vec(&local)
                .map(Some)
                .map_err(|e| e.to_string())
        }
    }
}

/// Redacted diagnostics for POST /Catalog/GetPublishedItem. Never logs the
/// item ID or the body, only its structural state.
fn id_from_object<'a>(object: &'a serde_json::Map<String, Value>) -> Option<&'a str> {
    // Cosmos uses case-insensitive System.Text.Json deserialization.
    // Do not scan unrelated UUID-like values, such as user or session IDs.
    object
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("itemid"))
        .or_else(|| object.iter().find(|(k, _)| k.eq_ignore_ascii_case("id")))
        .and_then(|(_, v)| v.as_str())
}

pub fn catalog_id(body: &[u8]) -> Result<String, &'static str> {
    if body.is_empty() {
        return Err("empty_body");
    }
    let request: Value = serde_json::from_slice(body).map_err(|_| "invalid_json")?;
    let root = request.as_object().ok_or("not_object")?;
    let id = id_from_object(root)
        .or_else(|| {
            ["Item", "Request", "Data", "Payload"]
                .iter()
                .find_map(|key| {
                    root.iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case(key))
                        .and_then(|(_, v)| v.as_object())
                        .and_then(id_from_object)
                })
        })
        .ok_or("missing_itemid")?;
    if !is_uuid(id) {
        return Err("invalid_itemid");
    }
    Ok(id.to_owned())
}

/// Redacted POST classification; never logs requested item UUID or payload.
pub fn catalog_request_state(body: &[u8]) -> &'static str {
    match catalog_id(body) {
        Ok(_) => "valid_itemid",
        Err(reason) => reason,
    }
}

fn is_uuid(v: &str) -> bool {
    v.len() == 36
        && v.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn append() -> Rule {
        Rule {
            name: "inventory".into(),
            enabled: true,
            host: "example.test".into(),
            path: "/inventory".into(),
            method: "GET".into(),
            mode: Mode::AppendArray,
            pointer: "/result/items".into(),
            file: "items.json".into(),
            identity_key: "id".into(),
            catalog_directory: String::new(),
            official_preview_ids: String::new(),
            local_only: false,
        }
    }
    #[test]
    fn preserves_original_entitlements_and_deduplicates() {
        let dir = std::env::temp_dir().join(format!("cosmetics_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("items.json"), r#"[{"id":"owned"},{"id":"local"}]"#).unwrap();
        let src =
            br#"{"result":{"items":[{"id":"owned","receipt":"genuine"}],"etag":"unchanged"}}"#;
        let value: Value =
            serde_json::from_slice(&overlay(&dir, &append(), b"", src).unwrap().unwrap()).unwrap();
        assert_eq!(value["result"]["items"].as_array().unwrap().len(), 2);
        assert_eq!(value["result"]["items"][0]["receipt"], "genuine");
        assert_eq!(value["result"]["etag"], "unchanged");
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn local_only_config_ignores_legacy_files_and_builtin_cosmos_catalog() {
        let dir = std::env::temp_dir().join(format!("cosmetics_only_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("items.json"), r#"[{"id":"old-cloud-item"}]"#).unwrap();
        let mut rule = append();
        rule.name = "dressing_room_capes".into();
        rule.local_only = true;
        rule.pointer = "/result/layout/0/rows/0/components/0/items".into();
        let body=br#"{"result":{"layout":[{"rows":[{"components":[{"type":"itemListComp","items":[]}]}]}]}}"#;
        // No registered ZIPs in a unit test: no extra item should be injected.
        assert!(overlay(&dir, &rule, b"", body).unwrap().is_none());
        rule.mode = Mode::CatalogItem;
        rule.catalog_directory = "legacy_catalog_that_does_not_exist".into();
        let id = "5bba5540-377b-4ec2-9633-4551c4e82e37";
        let req = format!(r#"{{"itemid":"{id}"}}"#);
        assert!(overlay(&dir, &rule, req.as_bytes(), b"").unwrap().is_none());
        let _ = fs::remove_dir_all(dir);
    }
    #[test]
    fn unknown_inventory_schema_fails_without_mutating_input() {
        let dir =
            std::env::temp_dir().join(format!("cosmetics_test_schema_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("items.json"), r#"[{"id":"local"}]"#).unwrap();
        let src = br#"{"result":{"other":[]}}"#;
        assert!(overlay(&dir, &append(), b"", src).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn catalog_item_only_replaces_matching_registered_uuid() {
        let dir =
            std::env::temp_dir().join(format!("cosmetics_test_catalog_{}", std::process::id()));
        let folder = dir.join("catalog");
        fs::create_dir_all(&folder).unwrap();
        let id = "5bba5540-377b-4ec2-9633-4551c4e82e37";
        fs::write(folder.join(format!("{id}.json")),
            format!(r#"{{"data":{{"Item":{{"Id":"{id}","Contents":[{{"Type":"personabinary","Url":"https://cosmetics.invalid/personabinary/{id}.zip"}}]}}}}}}"#)).unwrap();
        let mut rule = append();
        rule.mode = Mode::CatalogItem;
        rule.catalog_directory = "catalog".into();
        let original = br#"{"status":"not found"}"#;
        let request = format!(r#"{{"itemid":"{id}"}}"#);
        let result = overlay(&dir, &rule, request.as_bytes(), original)
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(value["data"]["Item"]["Id"], id);
        let unknown = br#"{"itemid":"00000000-0000-0000-0000-000000000001"}"#;
        assert!(overlay(&dir, &rule, unknown, original).unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn list_append_never_reduces_server_advertised_capacity() {
        let dir = std::env::temp_dir().join(format!("cosmetics_caps_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("items.json"), r#"[{"id":"added"}]"#).unwrap();
        let mut rule = append();
        rule.pointer = "/result/layout/0/rows/0/components/0/items".into();
        let original=br#"{"result":{"layout":[{"rows":[{"components":[{"type":"itemListComp","items":[],"totalItems":400,"customStoreRowConfiguration":{"maxOffers":400}}]}]}]}}"#;
        let output: Value =
            serde_json::from_slice(&overlay(&dir, &rule, b"", original).unwrap().unwrap()).unwrap();
        let list = &output["result"]["layout"][0]["rows"][0]["components"][0];
        assert_eq!(list["totalItems"], 400);
        assert_eq!(list["customStoreRowConfiguration"]["maxOffers"], 400);
        let _ = fs::remove_dir_all(dir);
    }
    #[test]
    fn official_cape_local_owned_projection_preserves_original_receipts_and_other_items() {
        let dir = std::env::temp_dir().join(format!("cosmetics_official_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let original_id = "5bba5540-377b-4ec2-9633-4551c4e82e37";
        let custom_id = "e847fc66-b988-4065-bdc9-fa8fd17457b2";
        fs::write(
            dir.join("ids.json"),
            format!(r#"["{original_id}","{custom_id}"]"#),
        )
        .unwrap();
        fs::write(dir.join("items.json"),format!(r#"[
            {{"id":"{custom_id}","creatorName":"Other","pieceType":"persona_capes","ownership":"NotOwned"}}
        ]"#)).unwrap();
        let mut rule = append();
        rule.official_preview_ids = "ids.json".into();
        rule.pointer = "/result/layout/0/rows/0/components/0/items".into();
        let original = format!(
            r#"{{"result":{{"layout":[{{"rows":[{{"components":[{{"type":"itemListComp","items":[{{"id":"{original_id}","creatorName":"Minecraft","pieceType":"persona_capes","ownership":"NotOwned","receipt":"untouched"}}]}}]}}]}}]}}}}"#
        );
        let outcome: Value = serde_json::from_slice(
            &overlay(&dir, &rule, b"", original.as_bytes())
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let items = outcome
            .pointer("/result/layout/0/rows/0/components/0/items")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(items[0]["ownership"], "Purchased");
        assert_eq!(items[0]["receipt"], "untouched");
        assert_eq!(items[1]["ownership"], "NotOwned");
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn inventory_probe_is_read_only() {
        let mut rule = append();
        rule.mode = Mode::InventoryProbe;
        let body = br#"{"result":{"items":[{"id":"owned","receipt":"untouched"}]}}"#;
        assert!(overlay(Path::new("."), &rule, b"", body).unwrap().is_none());
    }
    #[test]
    fn list_append_updates_render_limit_and_keeps_original_entries() {
        let dir = std::env::temp_dir().join(format!("cosmetics_item_list_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("items.json"), r#"[{"id":"new"}]"#).unwrap();
        let mut rule = append();
        rule.pointer = "/result/layout/0/rows/0/components/0/items".into();
        let original=br#"{"result":{"layout":[{"rows":[{"components":[{"type":"itemListComp","items":[{"id":"owned","ownership":"Purchased"}],"totalItems":1,"customStoreRowConfiguration":{"maxOffers":1}}]}]}]}}"#;
        let modified = overlay(&dir, &rule, b"", original).unwrap().unwrap();
        let value: Value = serde_json::from_slice(&modified).unwrap();
        let component = &value["result"]["layout"][0]["rows"][0]["components"][0];
        assert_eq!(component["totalItems"], 2);
        assert_eq!(component["customStoreRowConfiguration"]["maxOffers"], 2);
        assert_eq!(component["items"][0]["ownership"], "Purchased");
        assert_eq!(component["items"][1]["id"], "new");
        let _ = fs::remove_dir_all(dir);
    }
    #[test]
    fn persona_profile_adds_a_navigation_item_without_erasing_ownership() {
        let dir = std::env::temp_dir().join(format!("cosmetics_profile_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("profile.json"),
            br#"[{"id":"nav","title":"Local capes","ownership":"Purchased"}]"#,
        )
        .unwrap();
        let mut rule = append();
        rule.mode = Mode::PersonaProfile;
        rule.file = "profile.json".into();
        let original=br#"{"result":{"layout":[{"rows":[{"controlId":"StoreRow","components":[{"type":"dropdownOptionComp","dropdownId":0},{"type":"itemListComp","items":[{"id":"official","ownership":"Purchased"}],"totalItems":1,"customStoreRowConfiguration":{"maxOffers":1}}]}]}]}}"#;
        let output = overlay(&dir, &rule, b"", original).unwrap().unwrap();
        let value: Value = serde_json::from_slice(&output).unwrap();
        let list = &value["result"]["layout"][0]["rows"][0]["components"][1];
        assert_eq!(list["totalItems"], 2);
        assert_eq!(list["items"][0]["ownership"], "NotOwned");
        assert_eq!(list["items"][1]["ownership"], "Purchased");
        let _ = fs::remove_dir_all(dir);
    }
    #[test]
    fn catalog_accepts_cosmos_case_insensitive_id_and_nested_schemas() {
        let uuid = "5bba5540-377b-4ec2-9633-4551c4e82e37";
        for data in [
            format!(r#"{{"ItemId":"{uuid}"}}"#),
            format!(r#"{{"ITEMID":"{uuid}"}}"#),
            format!(r#"{{"Id":"{uuid}"}}"#),
            format!(r#"{{"Item":{{"Id":"{uuid}"}}}}"#),
            format!(r#"{{"request":{{"itemId":"{uuid}"}}}}"#),
        ] {
            assert_eq!(catalog_id(data.as_bytes()).unwrap(), uuid);
        }
        assert_eq!(
            catalog_id(br#"{"token":"5bba5540-377b-4ec2-9633-4551c4e82e37"}"#),
            Err("missing_itemid")
        );
        assert_eq!(catalog_id(br#"{"itemid":"bad"}"#), Err("invalid_itemid"));
    }
    #[test]
    fn debug_defaults_enabled_and_can_be_disabled() {
        let c: Config = serde_json::from_str(r#"{"rules":[]}"#).unwrap();
        assert!(c.debug.verbose && c.debug.response_shape);
        let d: Config =
            serde_json::from_str(r#"{"debug":{"verbose":false,"response_shape":false}}"#).unwrap();
        assert!(!d.debug.verbose && !d.debug.response_shape);
    }
    #[test]
    fn catalog_request_state_is_redacted_and_classifies_body() {
        assert_eq!(catalog_request_state(b""), "empty_body");
        assert_eq!(catalog_request_state(b"bad"), "invalid_json");
        assert_eq!(catalog_request_state(br#"{}"#), "missing_itemid");
        assert_eq!(
            catalog_request_state(br#"{"itemid":"oops"}"#),
            "invalid_itemid"
        );
        assert_eq!(
            catalog_request_state(br#"{"itemid":"5bba5540-377b-4ec2-9633-4551c4e82e37"}"#),
            "valid_itemid"
        );
    }
    #[test]
    fn missing_overlay_files_inject_no_builtin_cosmos_entries() {
        // BLoader 移植：内嵌 Cosmos 元数据预设已移除；外部文件缺失时
        // 披风页保持原样（仅本地披风可能注入），未知目录条目透传。
        let dir = std::env::temp_dir().join(format!("cosmetics_empty_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut rule = append();
        rule.name = "dressing_room_capes".into();
        rule.file = "overrides/capes_items.json".into();
        rule.pointer = "/result/layout/0/rows/0/components/0/items".into();
        let original=br#"{"result":{"layout":[{"rows":[{"controlId":"GridList","components":[{"type":"headerComp","text":{"value":"Owned","replacements":[]}},{"type":"itemListComp","items":[],"totalItems":0,"customStoreRowConfiguration":{"maxOffers":0,"visibleOffersPerRow":6}},{"type":"dispPreviewPieceComp"}]}]}]}}"#;
        assert!(
            overlay(&dir, &rule, b"", original).unwrap().is_none(),
            "no external overlay file and no local capes must keep the page unchanged"
        );
        rule.mode = Mode::CatalogItem;
        rule.catalog_directory = "overrides/catalog".into();
        let request = br#"{"itemid":"5bba5540-377b-4ec2-9633-4551c4e82e37"}"#;
        assert!(
            overlay(&dir, &rule, request, b"{}").unwrap().is_none(),
            "unknown catalog items pass through without embedded presets"
        );
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn path_and_host_are_exact_and_catalog_id_is_validated() {
        let c: Config = serde_json::from_str(r#"{"rules":[{"name":"x","host":"store.example","path":"/p","mode":"append_array","pointer":"/items","file":"items.json"}]}"#).unwrap();
        assert!(c.match_rule("STORE.EXAMPLE", "/p", "GET").is_some());
        assert!(c.match_rule("store.example.evil", "/p", "GET").is_none());
        assert!(c.match_rule("store.example", "/p?q=1", "GET").is_none());
        let market:Config=serde_json::from_str(r#"{"rules":[{"name":"capes","host":"store.mktpl.minecraft-services.net","path":"/api/v2.0/layout/pages/DressingRoom_Capes","mode":"append_array","file":"x"}]}"#).unwrap();
        assert!(
            market
                .match_rule(
                    "store.mktpl.minecraft-services.net",
                    "/api/v2.0/layout/pages/DressingRoom_Capes?lang=en-US",
                    "GET"
                )
                .is_some()
        );
        let both:Config=serde_json::from_str(r#"{"rules":[{"name":"capes","host":"store.mktpl.minecraft-services.net","path":"/api/v2.0/layout/pages/DressingRoom_Capes","method":"GET,POST","mode":"append_array","file":"x"}]}"#).unwrap();
        assert!(
            both.match_rule(
                "store.mktpl.minecraft-services.net",
                "/api/v2.0/layout/pages/DressingRoom_Capes",
                "POST"
            )
            .is_some()
        );
        assert!(
            both.match_rule(
                "store.mktpl.minecraft-services.net",
                "/api/v2.0/layout/pages/DressingRoom_Capes",
                "GET"
            )
            .is_some()
        );
        assert!(
            both.match_rule(
                "store.mktpl.minecraft-services.net",
                "/api/v2.0/layout/pages/DressingRoom_Capes",
                "PUT"
            )
            .is_none()
        );
        assert!(is_uuid("5bba5540-377b-4ec2-9633-4551c4e82e37"));
        assert!(!is_uuid("../.."));
    }
}
