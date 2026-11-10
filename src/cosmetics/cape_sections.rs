//! Cosmos-compatible DressingRoom_Capes sections, derived from the live page
//! instead of replacing its complete layout or maintaining a second catalog.
//! Existing official/owned rows and entitlement fields are never removed.
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SectionStats {
    pub official: usize,
    pub community: usize,
    pub local: usize,
    pub ownership_projected: usize,
}
impl SectionStats {
    pub fn changed(&self) -> bool {
        self.official + self.community + self.local + self.ownership_projected > 0
    }
}
fn official_creator(item: &Value) -> bool {
    matches!(
        item.get("creatorName").and_then(Value::as_str),
        Some("Minecraft" | "Minecraft Dungeons" | "Minecraft Dungeons II")
    )
}
fn item_components(row: &Value) -> Option<&Vec<Value>> {
    row.get("components")?.as_array()
}
fn prototype_row(rows: &[Value]) -> Option<Value> {
    rows.iter()
        .filter(|row| row.get("controlId").and_then(Value::as_str) == Some("GridList"))
        .filter(|row| {
            item_components(row).is_some_and(|parts| {
                parts
                    .iter()
                    .any(|c| c.get("type").and_then(Value::as_str) == Some("headerComp"))
                    && parts
                        .iter()
                        .any(|c| c.get("type").and_then(Value::as_str) == Some("itemListComp"))
            })
        })
        // Prefer a normal Cosmos cape section, not a link to Inventory.
        .find(|row| {
            item_components(row).is_some_and(|parts| {
                parts.iter().any(|c| {
                    c.get("type").and_then(Value::as_str) == Some("itemListComp")
                        && c.get("linksTo").is_none()
                })
            })
        })
        .cloned()
        .or_else(|| {
            rows.iter()
                .find(|row| {
                    row.get("controlId").and_then(Value::as_str) == Some("GridList")
                        && item_components(row).is_some_and(|parts| {
                            parts.iter().any(|c| {
                                c.get("type").and_then(Value::as_str) == Some("headerComp")
                            }) && parts.iter().any(|c| {
                                c.get("type").and_then(Value::as_str) == Some("itemListComp")
                            })
                        })
                })
                .cloned()
        })
}
fn build_row(prototype: &Value, title: &str, items: Vec<Value>) -> Result<Value, String> {
    let mut row = prototype.clone();
    let components = row
        .get_mut("components")
        .and_then(Value::as_array_mut)
        .ok_or("cape section template is missing components")?;
    // Do not clone the Owned section's Default button or other interactions.
    components.retain(|c| {
        matches!(
            c.get("type").and_then(Value::as_str),
            Some("headerComp" | "itemListComp" | "dispPreviewPieceComp")
        )
    });
    let n = items.len();
    for c in components {
        match c.get("type").and_then(Value::as_str) {
            Some("headerComp") => {
                let obj = c
                    .get_mut("text")
                    .and_then(Value::as_object_mut)
                    .ok_or("cape section header has no text")?;
                obj.insert("value".into(), Value::String(format!("{title} ({n})")));
                obj.insert("replacements".into(), json!([]));
            }
            Some("itemListComp") => {
                let obj = c.as_object_mut().ok_or("item list is not object")?;
                obj.remove("linksTo");
                obj.remove("linksToInfo");
                obj.insert("items".into(), Value::Array(items.clone()));
                obj.insert("totalItems".into(), Value::from(n));
                if let Some(config) = obj
                    .get_mut("customStoreRowConfiguration")
                    .and_then(Value::as_object_mut)
                {
                    config.insert("maxOffers".into(), Value::from(n));
                    if config.get("visibleOffersPerRow").and_then(Value::as_u64) == Some(0) {
                        config.insert("visibleOffersPerRow".into(), Value::from(6));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(row)
}
fn project_official(item: &mut Value, identity_key: &str, allowlist: &HashSet<String>) -> bool {
    let Some(id) = item.get(identity_key).and_then(Value::as_str) else {
        return false;
    };
    if !allowlist.contains(&id.to_ascii_lowercase())
        || !official_creator(item)
        || item.get("pieceType").and_then(Value::as_str) != Some("persona_capes")
        || item.get("ownership").and_then(Value::as_str) == Some("Purchased")
    {
        return false;
    }
    if let Some(obj) = item.as_object_mut() {
        obj.insert("ownership".into(), Value::String("Purchased".into()));
        return true;
    }
    false
}
/// Appends a small number of new sections while keeping every original row.
/// Does not set item entitlements unless the caller supplies an explicit,
/// previously enabled official-preview allowlist.
pub fn inject(
    root: &mut Value,
    imported: &[Value],
    local: &[Value],
    allowlist: &HashSet<String>,
    identity_key: &str,
) -> Result<SectionStats, String> {
    let rows = root
        .pointer_mut("/result/layout/0/rows")
        .and_then(Value::as_array_mut)
        .ok_or("cape page has no result.layout[0].rows")?;
    let mut seen = HashSet::new();
    let mut stats = SectionStats::default();
    for row in rows.iter_mut() {
        if let Some(components) = row.get_mut("components").and_then(Value::as_array_mut) {
            for comp in components {
                if comp.get("type").and_then(Value::as_str) != Some("itemListComp") {
                    continue;
                }
                if let Some(items) = comp.get_mut("items").and_then(Value::as_array_mut) {
                    for item in items {
                        if let Some(id) = item.get(identity_key).and_then(Value::as_str) {
                            seen.insert(id.to_ascii_lowercase());
                        }
                        if project_official(item, identity_key, allowlist) {
                            stats.ownership_projected += 1;
                        }
                    }
                }
            }
        }
    }
    let mut local_new = Vec::new();
    let mut official_new = Vec::new();
    let mut community_new = Vec::new();
    // Give user-created resources priority over an accidental duplicate
    // from a legacy Cosmos item file.
    for (entry, is_local) in local
        .iter()
        .map(|x| (x, true))
        .chain(imported.iter().map(|x| (x, false)))
    {
        let id = entry
            .get(identity_key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("cape item missing {identity_key} string"))?;
        if !seen.insert(id.to_ascii_lowercase()) {
            continue;
        }
        let mut copy = entry.clone();
        if project_official(&mut copy, identity_key, allowlist) {
            stats.ownership_projected += 1;
        }
        if is_local {
            local_new.push(copy)
        } else if official_creator(&copy) {
            official_new.push(copy)
        } else {
            community_new.push(copy)
        }
    }
    stats.official = official_new.len();
    stats.community = community_new.len();
    stats.local = local_new.len();
    if stats.official + stats.community + stats.local == 0 {
        return Ok(stats);
    }
    // The game's cape UI can change between a GridList and a plain
    // StoreRow/itemListComp layout. Do not discard already-scanned local capes
    // solely because the row wrapper does not match an older Cosmos fixture.
    // In that case, append the new entries to an existing item list in place,
    // keeping every original item, receipt, and ownership field unchanged.
    let Some(template) = prototype_row(rows) else {
        let new_count = official_new.len() + community_new.len() + local_new.len();
        let Some(component) = rows.iter_mut()
            .filter_map(|row| row.get_mut("components").and_then(Value::as_array_mut))
            .flat_map(|components| components.iter_mut())
            .find(|component| {
                component.get("type").and_then(Value::as_str) == Some("itemListComp")
                    && component.get("items").and_then(Value::as_array).is_some()
            })
        else {
            return Err("cape page has no compatible itemListComp; local items not injected".into());
        };
        let Some(component_map) = component.as_object_mut() else {
            return Err("cape itemListComp is not an object".into());
        };
        let Some(items) = component_map.get_mut("items").and_then(Value::as_array_mut) else {
            return Err("cape itemListComp items is not an array".into());
        };
        items.extend(official_new);
        items.extend(community_new);
        items.extend(local_new);
        let total = items.len();
        let previous = component_map.get("totalItems").and_then(Value::as_u64).unwrap_or(0);
        component_map.insert("totalItems".into(), Value::from(previous.max(total as u64)));
        if let Some(options) = component_map.get_mut("customStoreRowConfiguration")
            .and_then(Value::as_object_mut)
        {
            let max = options.get("maxOffers").and_then(Value::as_u64).unwrap_or(0);
            options.insert("maxOffers".into(), Value::from(max.max(total as u64)));
        }
        debug_assert!(new_count > 0);
        return Ok(stats);
    };
    let divider = rows
        .iter()
        .find(|row| row.get("controlId").and_then(Value::as_str) == Some("VerticalLineDivider"))
        .cloned();
    let anchor = rows.iter()
        .position(|row| {
            row.get("controlId").and_then(Value::as_str) == Some("GridList")
                && item_components(row).is_some_and(|components| components.iter().any(|component| {
                    component.get("type").and_then(Value::as_str) == Some("itemListComp")
                }))
        })
        .ok_or("cape page has no GridList")? + 1;
    let categories = [
        ("Official Capes - Catalog Only", official_new),
        ("Community Capes", community_new),
        ("My Local ZIP Capes", local_new),
    ];
    let mut new_rows = Vec::new();
    for (title, items) in categories {
        if items.is_empty() {
            continue;
        }
        if let Some(divider) = &divider {
            new_rows.push(divider.clone());
        }
        new_rows.push(build_row(&template, title, items)?);
    }
    rows.splice(anchor..anchor, new_rows);
    Ok(stats)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        json!({"result":{"layout":[{"rows":[
            {"controlId":"GridList","components":[
                {"type":"headerComp","text":{"value":"Owned","replacements":["cape"]}},
                {"type":"itemListComp","linksTo":"MultiItemPage_Inventory",
                 "items":[{"id":"owned","ownership":"Purchased","receipt":"real"}],
                 "totalItems":1,"customStoreRowConfiguration":{"maxOffers":1,"visibleOffersPerRow":0}},
                {"type":"dispPreviewPieceComp"},{"type":"personaDefaultButtonComp"}
            ]},
            {"controlId":"VerticalLineDivider","components":[{"type":"textureComp"}]}
        ]}]}})
    }
    #[test]
    fn distinct_official_community_local_rows_keep_owned_original() {
        let mut root = fixture();
        let official = json!({"id":"official","creatorName":"Minecraft",
            "pieceType":"persona_capes","ownership":"NotOwned"});
        let community = json!({"id":"community","creatorName":"BionicBen",
            "pieceType":"persona_capes","ownership":"NotOwned"});
        let local = json!({"id":"local","creatorName":"Player",
            "pieceType":"persona_capes","ownership":"Purchased"});
        let stats = inject(
            &mut root,
            &[official, community.clone(), community],
            &[local],
            &HashSet::new(),
            "id",
        )
        .unwrap();
        assert_eq!(
            stats,
            SectionStats {
                official: 1,
                community: 1,
                local: 1,
                ownership_projected: 0
            }
        );
        let rows = root
            .pointer("/result/layout/0/rows")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 8);
        let original = &rows[0]["components"][1];
        assert_eq!(original["items"][0]["receipt"], "real");
        assert_eq!(original["linksTo"], "MultiItemPage_Inventory");
        let sections: Vec<_> = rows
            .iter()
            .filter_map(|r| r.get("components"))
            .filter_map(Value::as_array)
            .filter(|comps| {
                comps
                    .iter()
                    .any(|c| c.get("type").and_then(Value::as_str) == Some("itemListComp"))
            })
            .collect();
        assert_eq!(sections.len(), 4);
        for comp in sections.iter().skip(1) {
            let header = comp
                .iter()
                .find(|c| c.get("type") == Some(&json!("headerComp")))
                .unwrap();
            let items = comp
                .iter()
                .find(|c| c.get("type") == Some(&json!("itemListComp")))
                .unwrap();
            assert!(header["text"]["value"].as_str().unwrap().contains("Capes"));
            assert!(items.get("linksTo").is_none());
            assert_eq!(items["totalItems"], 1);
        }
        assert_eq!(sections[1][1]["items"][0]["ownership"], "NotOwned");
        assert_eq!(sections[3][1]["items"][0]["ownership"], "Purchased");
        // Repeat injection must never re-register duplicate capes.
        let second = inject(
            &mut root,
            &[json!({"id":"official","creatorName":"Minecraft"})],
            &[json!({"id":"local","creatorName":"Player"})],
            &HashSet::new(),
            "id",
        )
        .unwrap();
        assert!(!second.changed());
    }
    #[test]
    fn local_capes_still_appear_when_game_uses_store_row_instead_of_grid_list() {
        let mut root = json!({"result":{"layout":[{"rows":[
            {"controlId":"StoreRow","components":[
                {"type":"itemListComp","items":[{"id":"owned","ownership":"Purchased","receipt":"real"}],
                 "totalItems":1,"customStoreRowConfiguration":{"maxOffers":1}}
            ]}
        ]}]}});
        let local = json!({"id":"local-import","creatorName":"Local","ownership":"NotOwned",
            "pieceType":"persona_capes"});
        let stats = inject(&mut root, &[], &[local], &HashSet::new(), "id").unwrap();
        assert_eq!(stats.local, 1);
        let component = &root["result"]["layout"][0]["rows"][0]["components"][0];
        assert_eq!(component["items"].as_array().unwrap().len(), 2);
        assert_eq!(component["items"][0]["receipt"], "real");
        assert_eq!(component["items"][1]["id"], "local-import");
        assert_eq!(component["totalItems"], 2);
        assert_eq!(component["customStoreRowConfiguration"]["maxOffers"], 2);
        let repeat = inject(&mut root, &[], &[json!({"id":"local-import"})], &HashSet::new(), "id").unwrap();
        assert!(!repeat.changed());
    }

    #[test]
    fn only_explicit_allowlist_can_project_official_purchased() {
        let mut root = fixture();
        let item = json!({"id":"cape","creatorName":"Minecraft",
            "pieceType":"persona_capes","ownership":"NotOwned"});
        let ids = HashSet::from(["cape".to_owned()]);
        let stats = inject(&mut root, &[item], &[], &ids, "id").unwrap();
        assert_eq!(stats.ownership_projected, 1);
        let rows = root
            .pointer("/result/layout/0/rows")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(
            rows[2]["components"][1]["items"][0]["ownership"],
            "Purchased"
        );
    }
    #[test]
    fn unknown_page_keeps_original_in_caller() {
        let mut root =
            json!({"result":{"layout":[{"rows":[{"controlId":"Other","components":[]}]}]}});
        let snapshot = root.clone();
        let result = inject(&mut root, &[json!({"id":"x"})], &[], &HashSet::new(), "id");
        assert!(result.is_err());
        // Failed data is on a working copy, never shipped upstream.
        assert_eq!(root, snapshot);
    }
}
