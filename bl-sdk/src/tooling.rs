use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlManifest {
    pub id: String,
    pub name: String,
    pub entry: String,
    #[serde(rename = "type")]
    pub mod_type: String,
    pub api_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requires_symbol_pack: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_symbols: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceEntry {
    pub path: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedManifestBundle {
    pub manifest: BlManifest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ResourceEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlPackageMetadata {
    #[serde(default)]
    pub mod_id: Option<String>,
    #[serde(default)]
    pub mod_name: Option<String>,
    #[serde(default)]
    pub api_version: Option<u32>,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub manifest_type: Option<String>,
    #[serde(default)]
    pub resource_dirs: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub requires_symbol_pack: bool,
    #[serde(default)]
    pub required_symbols: Vec<String>,
    #[serde(default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize)]
struct CargoToml {
    package: CargoPackage,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    version: Option<String>,
    description: Option<String>,
    homepage: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    #[serde(default)]
    metadata: CargoPackageMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct CargoPackageMetadata {
    #[serde(default)]
    bl: BlPackageMetadata,
}

impl BlPackageMetadata {
    fn into_manifest(self, package_name: &str, package: &CargoPackage) -> BlManifest {
        let mod_id = self
            .mod_id
            .unwrap_or_else(|| package_name.replace('_', "."));
        let mod_name = self
            .mod_name
            .or_else(|| Some(package_name.to_string()))
            .unwrap_or_else(|| package_name.to_string());
        let entry = self
            .entry
            .unwrap_or_else(|| format!("{}.dll", package_name.replace('-', "_")));

        BlManifest {
            id: mod_id,
            name: mod_name,
            entry,
            mod_type: self.manifest_type.unwrap_or_else(|| "BL".to_string()),
            api_version: self.api_version.unwrap_or(1),
            description: package.description.clone(),
            version: package.version.clone(),
            authors: package.authors.clone(),
            homepage: package.homepage.clone(),
            requires_symbol_pack: self.requires_symbol_pack,
            required_symbols: self.required_symbols,
        }
    }
}

pub fn load_package_metadata(cargo_toml_path: &Path) -> Result<BlPackageMetadata, String> {
    let text = fs::read_to_string(cargo_toml_path)
        .map_err(|error| format!("failed to read {}: {}", cargo_toml_path.display(), error))?;
    let cargo = toml::from_str::<CargoToml>(&text)
        .map_err(|error| format!("failed to parse {}: {}", cargo_toml_path.display(), error))?;
    Ok(cargo.package.metadata.bl)
}

pub fn generate_manifest_bundle(cargo_toml_path: &Path) -> Result<GeneratedManifestBundle, String> {
    let text = fs::read_to_string(cargo_toml_path)
        .map_err(|error| format!("failed to read {}: {}", cargo_toml_path.display(), error))?;
    let cargo = toml::from_str::<CargoToml>(&text)
        .map_err(|error| format!("failed to parse {}: {}", cargo_toml_path.display(), error))?;
    let package_dir = cargo_toml_path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", cargo_toml_path.display()))?;
    let metadata = cargo.package.metadata.bl.clone();
    let manifest = metadata
        .clone()
        .into_manifest(&cargo.package.name, &cargo.package);
    let resources = collect_resources(package_dir, &metadata);
    Ok(GeneratedManifestBundle {
        manifest,
        resources,
    })
}

pub fn write_manifest_bundle(
    bundle: &GeneratedManifestBundle,
    output_dir: &Path,
) -> Result<PathBuf, String> {
    fs::create_dir_all(output_dir)
        .map_err(|error| format!("failed to create {}: {}", output_dir.display(), error))?;
    let output_path = output_dir.join("manifest.json");
    let json = serde_json::to_string_pretty(bundle)
        .map_err(|error| format!("failed to serialize manifest bundle: {}", error))?;
    fs::write(&output_path, json)
        .map_err(|error| format!("failed to write {}: {}", output_path.display(), error))?;
    Ok(output_path)
}

pub fn collect_resources(package_dir: &Path, metadata: &BlPackageMetadata) -> Vec<ResourceEntry> {
    let mut entries = Vec::new();
    let mut roots = metadata.resource_dirs.clone();
    if roots.is_empty() {
        roots.push("assets".to_string());
        roots.push("data".to_string());
        roots.push("resource_pack".to_string());
    }

    for root in roots {
        let dir = package_dir.join(&root);
        if !dir.exists() {
            continue;
        }
        collect_resource_dir(&dir, &dir, &mut entries);
    }

    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries
}

fn collect_resource_dir(root: &Path, current: &Path, entries: &mut Vec<ResourceEntry>) {
    let Ok(read_dir) = fs::read_dir(current) else {
        return;
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_resource_dir(root, &path, entries);
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let kind = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .map(|value| match value.as_str() {
                "json" => "json",
                "lang" => "lang",
                "png" => "texture",
                _ => "file",
            })
            .unwrap_or("file");
        entries.push(ResourceEntry {
            path: relative.to_string_lossy().replace('\\', "/"),
            kind: kind.to_string(),
        });
    }
}
