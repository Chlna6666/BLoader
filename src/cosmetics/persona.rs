//! Local opaque Persona ZIP response source.
//! Does not interpret images, decrypt proprietary assets or forge signatures.
use serde_json::Value;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

pub const LOCAL_HOST: &str = "cosmetics.invalid";
pub const LOCAL_PREFIX: &str = "/personabinary/";
pub const MAX_ZIP_BYTES: usize = 64 * 1024 * 1024;

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
pub struct ZipPayload {
    pub bytes: Vec<u8>,
    pub resource_uuid: String,
}
pub fn load(root: &Path, path: &str) -> Result<Option<ZipPayload>, String> {
    let Some(id) = resource_uuid(path) else {
        return Ok(None);
    };
    let file_path = root
        .join("overrides")
        .join("persona")
        .join(format!("{id}.zip"));
    if !file_path.is_file() {
        return Ok(None);
    }
    let len = fs::metadata(&file_path).map_err(|e| e.to_string())?.len();
    if len == 0 || len > MAX_ZIP_BYTES as u64 {
        return Err("Persona ZIP must be nonempty and at most 64 MiB".into());
    }
    let mut archive = zip::ZipArchive::new(File::open(&file_path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("invalid ZIP: {e}"))?;
    if archive.len() > 4096 {
        return Err("Persona ZIP has too many files".into());
    }
    // Only inspect a small plaintext manifest, not binary or encrypted cape resources.
    let mut manifest_file = archive
        .by_name("manifest.json")
        .map_err(|e| format!("missing plain manifest.json: {e}"))?;
    if manifest_file.size() > 256 * 1024 {
        return Err("manifest too large".into());
    }
    let mut plain = Vec::new();
    manifest_file
        .by_ref()
        .take(256 * 1024 + 1)
        .read_to_end(&mut plain)
        .map_err(|e| format!("manifest read failed: {e}"))?;
    drop(manifest_file);
    let manifest: Value =
        serde_json::from_slice(&plain).map_err(|e| format!("invalid persona manifest: {e}"))?;
    let modules = manifest
        .get("modules")
        .and_then(Value::as_array)
        .ok_or("manifest has no modules array")?;
    if !modules
        .iter()
        .any(|m| m.get("type").and_then(Value::as_str) == Some("persona_piece"))
    {
        return Err("manifest has no persona_piece module".into());
    }
    drop(archive);
    let bytes = fs::read(&file_path).map_err(|e| format!("reading Persona ZIP failed: {e}"))?;
    Ok(Some(ZipPayload {
        bytes,
        resource_uuid: id.to_string(),
    }))
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
