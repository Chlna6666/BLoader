//! Privacy-safe endpoint diagnostics. Never persist raw query strings, request
//! headers, bodies, response JSON, tokens, XUIDs, or arbitrary URL segments.

/// Display only fixed, allowlisted API categories and names.
pub fn route_label(host: &str, path: &str, verb: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or("");
    let host = match host.to_ascii_lowercase().as_str() {
        "store.mktpl.minecraft-services.net" => "store",
        "20ca2.playfabapi.com" => "playfab",
        "entitlements.mktpl.minecraft-services.net" => "entitlements",
        _ => "other",
    };
    let operation = match host {
        "store" => {
            if let Some(page) = path.strip_prefix("/api/v2.0/layout/pages/") {
                match page {
                    "DressingRoom_Capes" => "DressingRoom_Capes",
                    "DressingRoom_PersonaProfile" => "DressingRoom_PersonaProfile",
                    "MultiItemPage_PersonaSkinSelector" => "MultiItemPage_PersonaSkinSelector",
                    "MultiItemPage_PersonaCharacterCreator" => {
                        "MultiItemPage_PersonaCharacterCreator"
                    }
                    _ if page.starts_with("PersonaCreatorPage_") => "PersonaCreatorPage_<redacted>",
                    _ if page.starts_with("DressingRoom_") => "DressingRoom_<other>",
                    _ if page.starts_with("MultiItemPage_") => "MultiItemPage_<other>",
                    _ => "layout/<other>",
                }
            } else {
                "store/<other>"
            }
        }
        "playfab" => match path {
            "/Catalog/GetPublishedItem" => "Catalog/GetPublishedItem",
            "/Catalog/Search" => "Catalog/Search",
            _ if path.starts_with("/Catalog/") => "Catalog/<other>",
            _ => "playfab/<other>",
        },
        "entitlements" => match path {
            "/api/v1.0/player/inventory" => "player/inventory",
            _ => "entitlements/<other>",
        },
        _ => "other",
    };
    // Method never comes from a user-controlled query string.
    let method = match verb.to_ascii_uppercase().as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    };
    format!("{method} {host}/{operation}")
}

/// Collect only an allowlist of key shapes; no key or value from the raw
/// request JSON is copied into logs.
pub fn catalog_shape(body: &[u8]) -> &'static str {
    if body.is_empty() {
        return "empty";
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return "invalid_json";
    };
    let Some(object) = value.as_object() else {
        return "not_object";
    };
    if object.keys().any(|s| s.eq_ignore_ascii_case("itemid")) {
        return "top_itemid";
    }
    if object.keys().any(|s| s.eq_ignore_ascii_case("id")) {
        return "top_id";
    }
    for key in ["item", "request", "data", "payload"] {
        if let Some(obj) = object
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .and_then(|(_, v)| v.as_object())
        {
            if obj.keys().any(|s| s.eq_ignore_ascii_case("itemid")) {
                return "nested_itemid";
            }
            if obj.keys().any(|s| s.eq_ignore_ascii_case("id")) {
                return "nested_id";
            }
        }
    }
    "unknown_fields"
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_never_leak_queries_or_account_suffixes() {
        assert_eq!(
            route_label(
                "store.mktpl.minecraft-services.net",
                "/api/v2.0/layout/pages/PersonaCreatorPage_master_player_account!secret?token=abc",
                "GET"
            ),
            "GET store/PersonaCreatorPage_<redacted>"
        );
        assert_eq!(
            route_label(
                "entitlements.mktpl.minecraft-services.net",
                "/api/v1.0/player/inventory?includeReceipt=true&token=abc",
                "GET"
            ),
            "GET entitlements/player/inventory"
        );
        assert_eq!(
            route_label("20ca2.playfabapi.com", "/Catalog/GetPublishedItem", "POST"),
            "POST playfab/Catalog/GetPublishedItem"
        );
    }
    #[test]
    fn shapes_are_allowlisted_only() {
        assert_eq!(catalog_shape(br#"{"ItemId":"x"}"#), "top_itemid");
        assert_eq!(catalog_shape(br#"{"Item":{"Id":"x"}}"#), "nested_id");
        assert_eq!(catalog_shape(br#"{"token":"secret"}"#), "unknown_fields");
    }
}
