//! Local opaque Persona ZIP response source.
//! Does not interpret images, decrypt proprietary assets or forge signatures.
use serde_json::Value;
pub const LOCAL_HOST: &str = "cosmetics.invalid";
pub const LOCAL_PREFIX: &str = "/personabinary/";

pub fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}
pub fn resource_uuid(path: &str) -> Option<&str> {
    let id = path.strip_prefix(LOCAL_PREFIX)?.strip_suffix(".zip")?;
    is_uuid(id).then_some(id)
}

/// BLoader 移植：仅接受本地虚拟资源 URL。Cosmos CDN 白名单已随
/// "取消内置 Bedrock-Cosmos 资源" 一并移除——目录元数据不允许再指向
/// 任何第三方资产主机。
pub fn validate_local_catalog_urls(value: &Value) -> Result<(), String> {
    let item = value.pointer("/data/Item").ok_or("missing data.Item")?;
    let contents = item
        .get("Contents")
        .and_then(Value::as_array)
        .ok_or("missing Contents")?;
    if contents.is_empty() {
        return Err("empty Contents".into());
    }
    for resource in contents {
        if resource.get("Type").and_then(Value::as_str) != Some("personabinary") {
            return Err("Persona resource type must be personabinary".into());
        }
        let url = resource
            .get("Url")
            .and_then(Value::as_str)
            .ok_or("missing Url")?;
        if let Some(path) = url.strip_prefix("https://cosmetics.invalid") {
            if resource_uuid(path).is_none() {
                return Err("invalid local Persona URL".into());
            }
        } else {
            return Err("catalog URL is not a local virtual resource".into());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn id_and_route_guard() {
        assert!(resource_uuid("/personabinary/5bba5540-377b-4ec2-9633-4551c4e82e37.zip").is_some());
        assert!(resource_uuid("/personabinary/../secret.zip").is_none());
        assert!(resource_uuid("/personabinary/not-a-uuid.zip").is_none());
    }
    #[test]
    fn only_local_zip_urls() {
        let good: Value = serde_json::from_str(r#"{"data":{"Item":{"Contents":[{"Type":"personabinary","Url":"https://cosmetics.invalid/personabinary/5bba5540-377b-4ec2-9633-4551c4e82e37.zip"}]}}}"#).unwrap();
        assert!(validate_local_catalog_urls(&good).is_ok());
        let mut bad = good.clone();
        bad["data"]["Item"]["Contents"][0]["Url"] =
            Value::String("https://example.net/surprise.zip".into());
        assert!(validate_local_catalog_urls(&bad).is_err());
        // Cosmos CDN 白名单已移除：第三方资产主机一律拒绝。
        bad["data"]["Item"]["Contents"][0]["Url"] =
            Value::String("https://capes.bedrock-cosmos.app/zips/AndroidCape.zip".into());
        assert!(validate_local_catalog_urls(&bad).is_err());
        bad["data"]["Item"]["Contents"][0]["Url"] =
            Value::String("https://capes.bedrock-cosmos.app/zips/../private.zip".into());
        assert!(validate_local_catalog_urls(&bad).is_err());
    }
}
