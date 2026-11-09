//! Bounded, redacted debug summaries for intercepted JSON responses.
//! Values (account IDs, receipts, tokens, item IDs) are never emitted.
use serde_json::Value;

fn count_rows(node: &Value) -> (usize, usize, usize) {
    let Some(layouts) = node.pointer("/result/layout").and_then(Value::as_array) else {
        return (0, 0, 0);
    };
    let mut rows = 0usize;
    let mut lists = 0usize;
    let mut items = 0usize;
    for layout in layouts {
        if let Some(rs) = layout.get("rows").and_then(Value::as_array) {
            rows += rs.len();
            for row in rs {
                if let Some(cs) = row.get("components").and_then(Value::as_array) {
                    for comp in cs {
                        if comp.get("type").and_then(Value::as_str) == Some("itemListComp") {
                            lists += 1;
                            items += comp
                                .get("items")
                                .and_then(Value::as_array)
                                .map_or(0, |v| v.len());
                        }
                    }
                }
            }
        }
    }
    (rows, lists, items)
}
pub fn json_summary(bytes: &[u8]) -> String {
    const MAX_PARSE: usize = 8 * 1024 * 1024;
    if bytes.len() > MAX_PARSE {
        return format!("json=oversize bytes={}", bytes.len());
    }
    let Ok(json) = serde_json::from_slice::<Value>(bytes) else {
        return format!("json=invalid_or_binary bytes={}", bytes.len());
    };
    let kind = match &json {
        Value::Object(_) => "object",
        Value::Array(_) => "array",
        _ => "scalar",
    };
    let (rows, lists, items) = count_rows(&json);
    let item_state = if json.pointer("/data/Item").is_some() {
        "present"
    } else {
        "absent"
    };
    let response_items = json
        .pointer("/result/items")
        .and_then(Value::as_array)
        .map_or(0, |x| x.len());
    let purchased_local_capes = json
        .pointer("/result/layout")
        .and_then(Value::as_array)
        .map(|layouts| {
            layouts
                .iter()
                .flat_map(|layout| {
                    layout
                        .get("rows")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .flat_map(|row| {
                    row.get("components")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .flat_map(|component| {
                    component
                        .get("items")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .filter(|item| {
                    item.get("creatorName").and_then(Value::as_str) == Some("Minecraft")
                        && item.get("pieceType").and_then(Value::as_str) == Some("persona_capes")
                        && item.get("ownership").and_then(Value::as_str) == Some("Purchased")
                })
                .count()
        })
        .unwrap_or(0);
    format!(
        "json={kind} bytes={} layout_rows={rows} item_lists={lists} listed_items={items} inventory_array_len={response_items} purchased_minecraft_capes_ui={purchased_local_capes} catalog_item={item_state}",
        bytes.len()
    )
}
pub fn redact_route_method(method: &str) -> &'static str {
    match method.to_ascii_uppercase().as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "HEAD" => "HEAD",
        "DELETE" => "DELETE",
        _ => "OTHER",
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summary_does_not_include_sensitive_data() {
        let body=br#"{"authorization":"secret","result":{"layout":[{"rows":[{"components":[{"type":"itemListComp","items":[{"id":"private-xuid","receipt":"private-receipt"}]}]}]}]},"data":{"Item":{"Id":"secret-product"}}}"#;
        let result = json_summary(body);
        assert!(result.contains("item_lists=1"));
        assert!(result.contains("listed_items=1"));
        for secret in ["secret", "private", "receipt", "xuid", "product"] {
            assert!(!result.contains(secret), "{result}");
        }
    }
}
