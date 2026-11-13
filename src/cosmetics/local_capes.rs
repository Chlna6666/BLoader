//! Standalone user-authored cape packs. No network, Cosmos, external catalog
//! files or global registration. BLoader 移植扩展：
//! - 支持多扫描根（mods/Cosmetics/capes 与游戏 com.mojang/capes 等）
//! - 每个根同时接受 ZIP 包与解包文件夹包（文件夹结构 = ZIP 内容平铺）
use image::{ImageFormat, Rgba, RgbaImage, imageops};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
#[cfg(test)]
use std::io::Write;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use zip::ZipArchive;
#[cfg(test)]
use zip::{ZipWriter, write::SimpleFileOptions};

const MAX_PACKS: usize = 128;
const MAX_PACK_FILE: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;
const MAX_IMAGE: usize = 8 * 1024 * 1024;
const MAX_MANIFEST: usize = 64 * 1024;
const MAX_NATIVE: usize = 64 * 1024 * 1024;
const MAX_PACK_MEMBERS: usize = 256;
const MAX_SCAN_DEPTH: usize = 4;

/// 读取一个披风包（ZIP 成员或解包文件夹）的统一视图，成员路径使用 `/` 分隔。
struct PackFiles {
    members: BTreeMap<String, Vec<u8>>,
}
impl PackFiles {
    fn from_zip(path: &Path) -> Result<Self, String> {
        let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
        if metadata.len() == 0 || metadata.len() > MAX_PACK_FILE {
            return Err("pack size exceeds 64MiB".into());
        }
        let mut archive = ZipArchive::new(File::open(path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("invalid outer ZIP: {e}"))?;
        if archive.len() > MAX_PACK_MEMBERS {
            return Err("too many ZIP members".into());
        }
        let mut members = BTreeMap::new();
        for index in 0..archive.len() {
            let mut file = archive.by_index(index).map_err(|e| e.to_string())?;
            if file.is_dir() {
                continue;
            }
            let name = file.name().map_err(|e| e.to_string())?.into_owned();
            if !safe_entry(&name) {
                continue;
            }
            if file.size() > MAX_PACK_FILE as u64 {
                return Err(format!("ZIP member {name} too large"));
            }
            let mut bytes = Vec::new();
            file.by_ref()
                .take(MAX_PACK_FILE as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("ZIP member {name} read failed: {e}"))?;
            members.insert(name, bytes);
        }
        Ok(Self { members })
    }
    fn from_dir(path: &Path) -> Result<Self, String> {
        let mut members = BTreeMap::new();
        let mut total = 0usize;
        fn walk(
            dir: &Path,
            prefix: &str,
            depth: usize,
            members: &mut BTreeMap<String, Vec<u8>>,
            total: &mut usize,
        ) -> Result<(), String> {
            if depth > MAX_SCAN_DEPTH {
                return Err("pack folder nesting too deep".into());
            }
            let entries = fs::read_dir(dir).map_err(|e| e.to_string())?;
            let mut children: Vec<_> = entries.flatten().collect();
            children.sort_by_key(|e| e.file_name());
            for entry in children {
                let file_type = entry.file_type().map_err(|e| e.to_string())?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let rel = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                if file_type.is_dir() {
                    walk(&entry.path(), &rel, depth + 1, members, total)?;
                } else if file_type.is_file() {
                    let metadata = entry.metadata().map_err(|e| e.to_string())?;
                    if metadata.len() > MAX_PACK_FILE as u64 {
                        return Err(format!("pack file {rel} too large"));
                    }
                    let bytes = fs::read(entry.path())
                        .map_err(|e| format!("pack file {rel} read failed: {e}"))?;
                    *total += bytes.len();
                    if *total as u64 > MAX_PACK_FILE {
                        return Err("pack size exceeds 64MiB".into());
                    }
                    members.insert(rel, bytes);
                }
            }
            Ok(())
        }
        walk(path, "", 0, &mut members, &mut total)?;
        if members.len() > MAX_PACK_MEMBERS {
            return Err("too many pack files".into());
        }
        Ok(Self { members })
    }
    fn len(&self) -> usize {
        self.members.len()
    }
    fn get(&self, name: &str) -> Option<&Vec<u8>> {
        self.members.get(name)
    }
}
/// 解包文件夹包的快速标记检查：至少包含一个可识别的包文件才参与解析，
/// 避免把 com.mojang/capes 下的无关目录记为拒绝项。
fn folder_has_pack_files(path: &Path) -> bool {
    for marker in [
        "manifest.json",
        "pack.json",
        "cape.json",
        "cape.png",
        "persona.zip",
    ] {
        if path.join(marker).is_file() {
            return true;
        }
    }
    false
}

// Resolution and UV are inferred by validate_png() at startup.
// Cape packs follow the v1 standard layout (manifest.json + cape.json +
// texts/*.lang); the retired dev `pack.json` and bare `cape.json` single-cape
// layouts are rejected instead of shimmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetType {
    PersonaZip,
    Png,
}
impl AssetType {
    pub fn content_type(self) -> &'static str {
        match self {
            Self::PersonaZip => "application/zip",
            Self::Png => "image/png",
        }
    }
}
pub struct LocalAsset {
    pub data: Vec<u8>,
    pub kind: AssetType,
}
#[derive(Default)]
pub struct Registry {
    items: Vec<Value>,
    catalogs: HashMap<String, Vec<u8>>,
    assets: HashMap<String, LocalAsset>,
    pub diagnostics: Vec<String>,
    pub discovered_packs: usize,
    pub discovered_roots: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub native_payloads: usize,
    pub generated_persona_payloads: usize,
}
static REGISTRY: OnceLock<Registry> = OnceLock::new();
pub fn initialize(roots: &[PathBuf], locale: &str) -> &'static Registry {
    REGISTRY.get_or_init(|| Registry::scan(roots, locale))
}
pub fn count() -> usize {
    REGISTRY.get().map_or(0, |r| r.items.len())
}
impl Registry {
    pub fn asset_count(&self) -> usize {
        self.assets.len()
    }
}
pub fn items() -> &'static [Value] {
    REGISTRY.get().map_or(&[], |r| r.items.as_slice())
}
pub fn catalog(id: &str) -> Option<&'static [u8]> {
    REGISTRY
        .get()?
        .catalogs
        .get(&id.to_ascii_lowercase())
        .map(Vec::as_slice)
}
/// Resolve only registered resources. WinHTTP request targets can carry
/// cache-busting or image-resize query parameters that are not part of the key.
pub fn canonical_asset_path(path: &str) -> &str {
    path.split(['?', '#']).next().unwrap_or(path)
}
pub fn asset(path: &str) -> Option<(&'static [u8], AssetType)> {
    let a = REGISTRY.get()?.assets.get(canonical_asset_path(path))?;
    Some((a.data.as_slice(), a.kind))
}
fn validate_png(bytes: &[u8]) -> Result<RgbaImage, String> {
    // Reject truncated PNGs rather than accepting only an IHDR header.
    // A malformed cape cannot be treated as loadable after ZIP validation.
    let reader = image::load_from_memory_with_format(bytes, ImageFormat::Png)
        .map_err(|e| format!("PNG decode failed: {e}"))?;
    let (w, h) = (reader.width(), reader.height());
    if w == 0 || h == 0 || w > 2048 || h > 2048 {
        return Err("invalid PNG dimensions".into());
    }
    Ok(reader.into_rgba8())
}
/// Normalize the Persona payload texture to 8-bit RGBA, without resizing.
/// The PNG decoder supports indexed/palette (color type 3) images, but
/// Bedrock's Persona texture path has not been proven to support palette PNGs.
/// The thumbnail already uses decoded RGBA; the Persona texture must match.
/// Preserve existing RGBA8 bytes exactly to avoid needless cache churn.
fn persona_rgba_png(original: &[u8], decoded: &RgbaImage) -> Result<(Vec<u8>, bool), String> {
    if original.len() > 26 && original[24] == 8 && original[25] == 6 {
        return Ok((original.to_vec(), false));
    }
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(decoded.clone())
        .write_to(&mut out, ImageFormat::Png)
        .map_err(|e| format!("Persona RGBA PNG encode failed: {e}"))?;
    if out.get_ref().len() > MAX_IMAGE {
        return Err("normalized Persona PNG exceeds 8 MiB".into());
    }
    Ok((out.into_inner(), true))
}
// The standard Bedrock geometry.cape declares a 64x32 atlas. Experimental
// Persona offers with 20x capes have not loaded on the target game build,
// even when the PNG is valid RGBA8. Use the full-resolution image for local
// thumbnails, but expose an UV-equivalent 64x32 *runtime* compatibility
// texture in the generated Persona. The ZIP/PNG on disk stays unchanged.
// Block-average each discrete UV texel independently: generic convolution
// across the atlas can bleed pixels between the outer, lining and edge faces.
fn cape_compat_rgba(decoded: &RgbaImage) -> RgbaImage {
    let (width, height) = decoded.dimensions();
    debug_assert!(width % 64 == 0 && height % 32 == 0 && width / 64 == height / 32);
    let scale = width / 64;
    let mut out = RgbaImage::new(64, 32);
    for y in 0..32 {
        for x in 0..64 {
            let mut premul = [0u64; 3];
            let mut total_alpha = 0u64;
            for sy in 0..scale {
                for sx in 0..scale {
                    let pixel = decoded.get_pixel(x * scale + sx, y * scale + sy).0;
                    let a = pixel[3] as u64;
                    total_alpha += a;
                    for channel in 0..3 {
                        premul[channel] += pixel[channel] as u64 * a;
                    }
                }
            }
            let n = (scale * scale) as u64;
            let a = ((total_alpha + n / 2) / n) as u8;
            let rgb = if total_alpha == 0 {
                [0, 0, 0]
            } else {
                [
                    ((premul[0] + total_alpha / 2) / total_alpha) as u8,
                    ((premul[1] + total_alpha / 2) / total_alpha) as u8,
                    ((premul[2] + total_alpha / 2) / total_alpha) as u8,
                ]
            };
            out.put_pixel(x, y, Rgba([rgb[0], rgb[1], rgb[2], a]));
        }
    }
    out
}
fn persona_texture_png(
    original: &[u8],
    decoded: &RgbaImage,
) -> Result<(Vec<u8>, bool, (u32, u32)), String> {
    if decoded.dimensions() == (64, 32) {
        let (png, converted) = persona_rgba_png(original, decoded)?;
        return Ok((png, converted, decoded.dimensions()));
    }
    let compatible = cape_compat_rgba(decoded);
    // Force a canonical RGBA8 PNG in the standard Persona atlas size.
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(compatible)
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|e| format!("Persona compatibility PNG encode failed: {e}"))?;
    Ok((output.into_inner(), true, (64, 32)))
}
fn fill_thumb_rect(canvas: &mut RgbaImage, x0: u32, y0: u32, x1: u32, y1: u32, color: Rgba<u8>) {
    for y in y0..y1 {
        for x in x0..x1 {
            canvas.put_pixel(x, y, color);
        }
    }
}
fn make_thumbnail(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let (w, h) = image.dimensions();
    // The Minecraft cape texture is a UV sheet, not a square marketplace
    // thumbnail. For 64x32 textures the OUTER back is x=1..11,y=1..17.
    let face = if w % 64 == 0 && h % 32 == 0 && w / 64 == h / 32 {
        let scale = w / 64;
        imageops::crop_imm(image, scale, scale, 10 * scale, 16 * scale).to_image()
    } else {
        image.clone()
    };
    // Preview the cape on the *back* of a neutral mannequin, not a flat
    // texture tile. For vanilla geometry.cape with a 180 degree cape-bone
    // rotation, the outer face is UV x=1..11; x=12..22 is the lining.
    // The original texture is never mirrored, and scale is derived at runtime.
    let resized = imageops::resize(&face, 34, 43, imageops::FilterType::Nearest);
    let mut canvas = RgbaImage::from_pixel(64, 64, Rgba([40, 43, 48, 255]));
    // Mannequin: neck/head, shoulders, arms and feet remain visible behind
    // the cape. Do not embed copyrighted game textures or a fake 'front'.
    fill_thumb_rect(&mut canvas, 25, 2, 39, 17, Rgba([129, 137, 145, 255]));
    fill_thumb_rect(&mut canvas, 25, 2, 39, 5, Rgba([157, 164, 171, 255]));
    fill_thumb_rect(&mut canvas, 10, 17, 54, 37, Rgba([93, 104, 113, 255]));
    fill_thumb_rect(&mut canvas, 7, 23, 15, 46, Rgba([119, 127, 134, 255]));
    fill_thumb_rect(&mut canvas, 49, 23, 57, 46, Rgba([119, 127, 134, 255]));
    fill_thumb_rect(&mut canvas, 19, 54, 29, 62, Rgba([80, 88, 96, 255]));
    fill_thumb_rect(&mut canvas, 35, 54, 45, 62, Rgba([80, 88, 96, 255]));
    // Small directional shadow and trim suggest depth, while cape front
    // pattern remains completely determined by the actual outer-back UV.
    fill_thumb_rect(&mut canvas, 13, 18, 50, 61, Rgba([29, 32, 38, 255]));
    imageops::overlay(&mut canvas, &resized, 15, 17);
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(canvas)
        .write_to(&mut out, ImageFormat::Png)
        .map_err(|e| format!("thumbnail encode failed: {e}"))?;
    Ok(out.into_inner())
}
fn uuid_from(seed: &[u8], kind: &str) -> String {
    let mut sha = Sha256::new();
    sha.update(b"Cosmetics/local-capes/v1/");
    sha.update(kind.as_bytes());
    sha.update([0]);
    sha.update(seed);
    let digest = sha.finalize();
    let mut raw = [0u8; 16];
    raw.copy_from_slice(&digest[..16]);
    raw[6] = (raw[6] & 0x0f) | 0x80; // v8 custom deterministic UUID
    raw[8] = (raw[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        raw[0],
        raw[1],
        raw[2],
        raw[3],
        raw[4],
        raw[5],
        raw[6],
        raw[7],
        raw[8],
        raw[9],
        raw[10],
        raw[11],
        raw[12],
        raw[13],
        raw[14],
        raw[15]
    )
}
fn valid_uuid(id: &str) -> bool {
    super::persona::is_uuid(id)
}
// Bedrock may cache installed Persona resources by pack UUID + version,
// independently of the CDN URL. Generated packs must not retain 1.0.0
// when their texture or generated metadata has changed. Keep the product
// ID and pack UUID stable; only the generated pack's content version changes.
// Native user-provided Persona versions are never replaced.
fn generated_pack_version(
    name: &str,
    author: &str,
    png: &[u8],
    author_version: [u32; 3],
) -> [u32; 3] {
    let mut hash = Sha256::new();
    hash.update(b"Cosmetics/generated-persona-cache-version/v4");
    hash.update(author_version[0].to_le_bytes());
    hash.update(author_version[1].to_le_bytes());
    hash.update(author_version[2].to_le_bytes());
    hash.update(name.as_bytes());
    hash.update([0]);
    hash.update(author.as_bytes());
    hash.update([0]);
    hash.update(png);
    let d = hash.finalize();
    let minor = u16::from_le_bytes([d[0], d[1]]) as u32;
    let patch = u16::from_le_bytes([d[2], d[3]]) as u32;
    [
        author_version[0],
        minor,
        if minor == 0 && patch == 0 { 1 } else { patch },
    ]
}
fn version_string(v: [u32; 3]) -> String {
    format!("{}.{}.{}", v[0], v[1], v[2])
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct NativeIdentity {
    uuid: String,
    version: String,
}
fn is_binary_bundle(bytes: &[u8]) -> Result<bool, String> {
    let zip =
        ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("persona ZIP invalid: {e}"))?;
    let bundled = zip.file_names().flatten().any(|name| name == "ppack0.zip");
    Ok(bundled)
}
fn validate_native(bytes: &[u8]) -> Result<NativeIdentity, String> {
    // Native Persona data can be a direct pack or a catalog personabinary
    // bundle enclosing ppack0.zip. Validate the inner manifest in both cases.
    let nested = if is_binary_bundle(bytes)? {
        let mut bundle = ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| format!("persona bundle invalid: {e}"))?;
        if bundle.len() > 64 {
            return Err("persona bundle has too many members".into());
        }
        let mut file = bundle.by_name("ppack0.zip").map_err(|e| e.to_string())?;
        if file.size() == 0 || file.size() > MAX_NATIVE as u64 {
            return Err("ppack0.zip exceeds Persona native size limit".into());
        }
        let mut inner = Vec::new();
        file.by_ref()
            .take(MAX_NATIVE as u64 + 1)
            .read_to_end(&mut inner)
            .map_err(|e| e.to_string())?;
        if inner.len() > MAX_NATIVE {
            return Err("ppack0.zip too large".into());
        }
        Some(inner)
    } else {
        None
    };
    let bytes = nested.as_deref().unwrap_or(bytes);
    let mut zip = ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("native persona.zip invalid: {e}"))?;
    if zip.len() > 1024 {
        return Err("native persona.zip has too many members".into());
    }
    let mut raw = Vec::new();
    {
        let mut file = zip
            .by_name("manifest.json")
            .map_err(|e| format!("persona manifest missing: {e}"))?;
        if file.size() > MAX_MANIFEST as u64 {
            return Err("persona manifest too large".into());
        }
        file.by_ref()
            .take(MAX_MANIFEST as u64 + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
    }
    let manifest: Value =
        serde_json::from_slice(&raw).map_err(|e| format!("persona manifest JSON invalid: {e}"))?;
    let module = manifest
        .get("modules")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter()
                .find(|m| m.get("type").and_then(Value::as_str) == Some("persona_piece"))
        })
        .ok_or("persona.zip must contain a persona_piece module")?;
    // The persona packIdentity refers to header.uuid, NOT the module UUID.
    // Native Bedrock Persona cache examples have distinct header/module UUIDs.
    if !module
        .get("uuid")
        .and_then(Value::as_str)
        .is_some_and(valid_uuid)
    {
        return Err("persona_piece module must have a valid uuid".into());
    }
    let header = manifest
        .get("header")
        .ok_or("native manifest missing header")?;
    let uuid = header
        .get("uuid")
        .and_then(Value::as_str)
        .filter(|id| valid_uuid(id))
        .ok_or("native persona header.uuid must be a UUID")?
        .to_ascii_lowercase();
    let version = match header.get("version") {
        Some(Value::Array(parts)) if parts.len() == 3 => {
            let parts: Option<Vec<u64>> = parts.iter().map(Value::as_u64).collect();
            let parts = parts.ok_or("native module version must contain numbers")?;
            format!("{}.{}.{}", parts[0], parts[1], parts[2])
        }
        Some(Value::String(s))
            if !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit() || c == b'.') =>
        {
            s.clone()
        }
        _ => return Err("persona_piece module version is missing or invalid".into()),
    };
    Ok(NativeIdentity { uuid, version })
}
fn experimental_persona_png(
    name: &str,
    author: &str,
    pack_uuid: &str,
    module_uuid: &str,
    png: &[u8],
    version: [u32; 3],
) -> Result<Vec<u8>, String> {
    super::persona_pack::generate(name, author, pack_uuid, module_uuid, png, version)
}

fn name_ok(input: &str) -> bool {
    !input.is_empty() && input.chars().count() <= 90 && !input.chars().any(char::is_control)
}

/// Empty authors are unspecified; malformed metadata is not silently discarded.
fn author_value(value: &Value) -> Result<Option<String>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let author = value.as_str().ok_or("author must be a string")?.trim();
    if author.is_empty() {
        return Ok(None);
    }
    if !name_ok(author) {
        return Err("invalid author".into());
    }
    Ok(Some(author.to_owned()))
}

/// Resource/skin pack convention: metadata.authors stores pack creators.
fn manifest_author(manifest: &Value) -> Result<Option<String>, String> {
    let Some(authors) = manifest.pointer("/metadata/authors") else {
        return author_value(&manifest["metadata"]["author"]);
    };
    let names = authors
        .as_array()
        .ok_or("metadata.authors must be an array")?
        .iter()
        .map(|value| {
            author_value(value)?.ok_or_else(|| "metadata.authors contains an empty name".to_owned())
        })
        .collect::<Result<Vec<_>, String>>()?;
    if names.is_empty() {
        return Ok(None);
    }
    let joined = names.join(", ");
    if !name_ok(&joined) {
        return Err("combined metadata.authors exceeds 90 characters".into());
    }
    Ok(Some(joined))
}
// Format v1 multi-cape archives declare a persistent package UUID, a stable
// per-cape key, and a version. No UUID is randomly generated at runtime.
#[derive(Deserialize)]
struct PackManifest {
    format_version: u32,
    id: String,
    name: String,
    #[serde(default)]
    author: String,
    #[serde(default = "default_pack_version")]
    version: String,
    capes: Vec<PackCape>,
}
#[derive(Deserialize)]
struct PackCape {
    id: String, // Stable key inside the package; not a random UUID
    name: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    uuid: Option<String>, // Optional explicit item UUID
    #[serde(default)]
    texture: Option<String>,
    #[serde(default)]
    icon: Option<String>, // Author-provided square icon for the thumbnail
    #[serde(default)]
    persona_zip: Option<String>,
    #[serde(default)]
    rarity: Option<String>,
    #[serde(default, rename = "startDate", alias = "start_date")]
    start_date: Option<String>,
}
fn default_pack_version() -> String {
    "1.0.0".to_owned()
}

/// Locale file lookup inside `texts/`: the configured locale first, then en_US.
fn localized_lang_text(files: &PackFiles, locale: &str) -> String {
    let normalized = locale.trim().replace('-', "_");
    for candidate in [normalized.as_str(), "en_US"] {
        if candidate.is_empty() {
            continue;
        }
        if let Some(bytes) = files.get(&format!("texts/{candidate}.lang")) {
            return String::from_utf8_lossy(bytes).into_owned();
        }
    }
    String::new()
}

fn standard_pack_manifest(
    files: &PackFiles,
    locale: &str,
    folder_name: Option<&str>,
) -> Result<Vec<u8>, String> {
    let cape = files.get("cape.json").ok_or("cape.json missing")?;
    if cape.len() > MAX_MANIFEST {
        return Err("cape manifest too large".into());
    }
    let sheet: Value =
        serde_json::from_slice(cape).map_err(|e| format!("cape.json invalid: {e}"))?;
    // Preserve the default author even when a bare descriptor becomes capes[].
    let sheet_author = author_value(&sheet["author"])?;
    // Two accepted layouts: the v1 pack (manifest.json + cape.json) and a
    // single-cape folder (cape.json + texture, no manifest). The single-cape
    // form derives its pack UUID from the folder name so the identity stays
    // stable across restarts and renames of the parent directory.
    let (id, name, version, header_author) = match files.get("manifest.json") {
        Some(manifest) => {
            if manifest.len() > MAX_MANIFEST {
                return Err("cape manifest too large".into());
            }
            let header: Value = serde_json::from_slice(manifest)
                .map_err(|e| format!("manifest.json invalid: {e}"))?;
            if header["format_version"].as_u64() != Some(1)
                || header["modules"].as_array().map_or(true, |modules| {
                    modules.len() != 1 || modules[0]["type"] != "cape_pack"
                })
            {
                return Err("unsupported cape manifest.json".into());
            }
            let id = header["header"]["uuid"]
                .as_str()
                .ok_or("cape pack UUID missing")?
                .to_string();
            let name = header["header"]["name"]
                .as_str()
                .ok_or("cape pack name missing")?
                .to_string();
            let version = header["header"]["version"]
                .as_array()
                .ok_or("cape pack version missing")?;
            if version.len() != 3 {
                return Err("cape pack version must have three parts".into());
            }
            let version = version
                .iter()
                .map(|part| {
                    part.as_u64()
                        .map(|v| v.to_string())
                        .ok_or("cape pack version invalid")
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(".");
            let author = manifest_author(&header)?;
            (id, name, version, author)
        }
        None => {
            // Bare folder layout: cape.json is a plain descriptor
            // (`{"name": "...", "author": "..."}`) next to the texture. The
            // pack UUID derives from the folder name so the identity stays
            // stable; the display name comes from the descriptor.
            let folder = folder_name.ok_or("manifest.json missing")?;
            let display = sheet["name"]
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(folder)
                .to_string();
            (
                uuid_from(folder.as_bytes(), "pack"),
                display,
                "1.0.0".to_string(),
                None,
            )
        }
    };
    // A descriptor-only cape.json carries no cape list; synthesize a single
    // entry from the pack name and the folder's texture.
    let sheet = if sheet
        .get("capes")
        .and_then(|value| value.as_array())
        .is_some_and(|capes| !capes.is_empty())
    {
        sheet
    } else {
        let texture = files
            .members
            .keys()
            .find(|name| name.to_ascii_lowercase().ends_with(".png"))
            .cloned()
            .unwrap_or_else(|| "cape.png".to_string());
        serde_json::json!({
            "localization_name": "capepack",
            "capes": [{
                "localization_name": "cape",
                "texture": texture,
                "name": name,
            }],
        })
    };
    let key = sheet["localization_name"]
        .as_str()
        .ok_or("cape pack localization_name missing")?;
    let text = localized_lang_text(files, locale);
    let localized = |key: &str| -> Option<String> {
        text.lines()
            .filter_map(|line| line.split_once('='))
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let author = sheet_author
        .or(header_author)
        .or_else(|| localized(&format!("capepack.{key}.by")))
        .unwrap_or_default();
    let entries = sheet["capes"].as_array().ok_or("cape list missing")?;
    let capes = entries
        .iter()
        .map(|entry| {
            let stable = entry["localization_name"]
                .as_str()
                .ok_or("cape localization_name missing")?;
            // texture stays optional: persona-only capes carry persona_zip instead.
            let texture = entry["texture"].as_str();
            let cape_author = author_value(&entry["author"])?;
            let display = localized(&format!("cape.{key}.{stable}"))
                .or_else(|| {
                    entry["name"]
                        .as_str()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| stable.to_owned());
            // cape.json 本身用 start_date；兼容上层工具写出的 startDate。
            let start_date = entry
                .get("start_date")
                .filter(|v| !v.is_null())
                .or_else(|| entry.get("startDate").filter(|v| !v.is_null()))
                .cloned()
                .unwrap_or(Value::Null);
            Ok(
                serde_json::json!({"id":stable,"name":display,"author":cape_author,"texture":texture,
            "icon":entry["icon"],"persona_zip":entry["persona_zip"],"uuid":entry["item_uuid"],
            "rarity":entry["rarity"],"startDate":start_date}),
            )
        })
        .collect::<Result<Vec<Value>, String>>()?;
    serde_json::to_vec(&serde_json::json!({"format_version":1,"id":id,
        "name":localized(&format!("capepack.{key}")).unwrap_or_else(||name.to_owned()),
        "author":author,"version":version,"capes":capes}))
    .map_err(|e| e.to_string())
}
const MAX_CAPES_PER_PACK: usize = 64;
const MAX_CAPES: usize = 512;
struct PreparedCape {
    id: String,
    item: Value,
    catalog: Vec<u8>,
    assets: Vec<(String, LocalAsset)>,
    native: bool,
    dimensions: Option<(u32, u32)>,
    png_color_type: Option<u8>,
    persona_rgba_normalized: bool,
    persona_texture_dimensions: Option<(u32, u32)>,
}
fn version_parts(version: &str) -> Result<[u32; 3], String> {
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|x| x.is_empty() || x.len() > 5 || !x.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("version must be major.minor.patch with numeric parts".into());
    }
    let mut result = [0u32; 3];
    for (i, part) in parts.into_iter().enumerate() {
        result[i] = part.parse().map_err(|_| "invalid version number")?;
        if result[i] > 65535 {
            return Err("version parts must not exceed 65535".into());
        }
    }
    Ok(result)
}
fn safe_entry(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 192
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && path.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && part.chars().all(|c| !c.is_control())
        })
}
/// Match Cosmos' known Persona offer rarity vocabulary; reject invalid user
/// values rather than injecting unexpected values into the Dressing Room.
fn normalized_rarity(value: Option<&str>) -> Result<&str, String> {
    let rarity = value.unwrap_or("common");
    if matches!(
        rarity,
        "common" | "uncommon" | "rare" | "epic" | "legendary"
    ) {
        Ok(rarity)
    } else {
        Err(format!("unsupported cape rarity: {rarity}"))
    }
}
/// Only put an author-provided publication date into the offer. A synthetic
/// placeholder date would not be real marketplace metadata.
fn valid_start_date(value: &str) -> bool {
    let b = value.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
        || b.iter()
            .enumerate()
            .any(|(i, c)| ![4, 7, 10, 13, 16, 19].contains(&i) && !c.is_ascii_digit())
    {
        return false;
    }
    let num = |a: usize, b: usize| value[a..b].parse::<u32>().unwrap_or(0);
    (2000..=9999).contains(&num(0, 4))
        && (1..=12).contains(&num(5, 7))
        && (1..=31).contains(&num(8, 10))
        && num(11, 13) < 24
        && num(14, 16) < 60
        && num(17, 19) < 60
}
fn stable_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
}
/// 从统一包视图中按成员名读取可选文件（ZIP 成员或解包文件夹文件）。
fn pack_file(
    files: &PackFiles,
    name: Option<&str>,
    max: usize,
    required: bool,
) -> Result<Option<Vec<u8>>, String> {
    let Some(name) = name else { return Ok(None) };
    if !safe_entry(name) {
        return Err(format!("invalid pack member path: {name}"));
    }
    match files.get(name) {
        Some(bytes) => {
            if bytes.is_empty() || bytes.len() > max {
                return Err(format!("pack member {name} has invalid size"));
            }
            Ok(Some(bytes.clone()))
        }
        None if required => Err(format!("pack member {name} missing")),
        None => Ok(None),
    }
}
impl Registry {
    pub fn scan(roots: &[PathBuf], locale: &str) -> Self {
        let mut reg = Self::default();
        let mut item_ids = HashSet::new();
        let mut asset_ids = HashSet::new();
        let mut total = 0usize;
        for root in roots {
            let dir = root.join("capes");
            // Scanning is read-only. The old create_dir_all() populated empty
            // com.mojang trees during bootstrap *after* file redirection was
            // installed, potentially changing which game-data root appears to
            // exist. A missing capes directory simply has no imported packs.
            if !dir.is_dir() {
                reg.diagnostics.push(format!(
                    "CAPE_ROOT_ABSENT path={} action=skip_without_creation",
                    dir.display()
                ));
                continue;
            }
            let mut packs: Vec<(PathBuf, bool)> = match fs::read_dir(&dir) {
                Ok(iter) => iter
                    .flatten()
                    .filter_map(|entry| {
                        let path = entry.path();
                        if path.is_dir() {
                            Some((path, true))
                        } else if path.extension().and_then(|x| x.to_str()).is_some_and(|x| {
                            x.eq_ignore_ascii_case("zip") || x.eq_ignore_ascii_case("mccapes")
                        }) {
                            Some((path, false))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>(),
                Err(e) => {
                    reg.diagnostics.push(format!(
                        "CAPE_ROOT path={} error=scan failed: {e}",
                        dir.display()
                    ));
                    continue;
                }
            };
            packs.sort();
            reg.discovered_roots += 1;
            reg.diagnostics.push(format!(
                "CAPE_ROOT path={} zips={} folders={}",
                dir.display(),
                packs.iter().filter(|(_, is_dir)| !*is_dir).count(),
                packs.iter().filter(|(_, is_dir)| *is_dir).count()
            ));
            for (path, is_dir) in packs {
                if reg.discovered_packs >= MAX_PACKS {
                    reg.diagnostics.push(format!(
                        "CAPE_SKIPPED pack limit reached (limit={MAX_PACKS})"
                    ));
                    break;
                }
                let label = path
                    .file_name()
                    .and_then(|x| x.to_str())
                    .unwrap_or("<invalid>")
                    .to_owned();
                if is_dir && !folder_has_pack_files(&path) {
                    reg.diagnostics.push(format!(
                        "CAPE_FOLDER_SKIPPED {label}: no manifest.json/cape.json/cape.png/persona.zip"));
                    continue;
                }
                reg.discovered_packs += 1;
                let result = Self::read_pack(&path, is_dir, locale);
                match result {
                    Ok((name, entries)) => {
                        let items = entries.len();
                        let bytes = entries
                            .iter()
                            .flat_map(|entry| entry.assets.iter())
                            .try_fold(0usize, |acc, (_, asset)| acc.checked_add(asset.data.len()))
                            .unwrap_or(usize::MAX);
                        let duplicate = entries.iter().any(|entry| {
                            item_ids.contains(&entry.id)
                                || entry
                                    .assets
                                    .iter()
                                    .any(|(path, _)| asset_ids.contains(path))
                        });
                        if duplicate
                            || items > MAX_CAPES.saturating_sub(reg.items.len())
                            || bytes > MAX_TOTAL_BYTES.saturating_sub(total)
                        {
                            reg.rejected += 1;
                            reg.diagnostics.push(format!(
                                "CAPE_REJECTED {label}: duplicate UUID/resource, too many capes or memory limit"));
                            continue;
                        }
                        total += bytes;
                        for (index, entry) in entries.into_iter().enumerate() {
                            let size = entry.dimensions.map_or("none".to_string(), |(w, h)| {
                                format!("{w}x{h} scale={}x", w / 64)
                            });
                            reg.diagnostics.push(format!(
                                "CAPE_REGISTERED {label} index={} texture={} persona_texture={} source_png_color_type={} persona_rgba_normalized={} payload={} game_accepted=UNVERIFIED{}",
                                index+1,size,entry.persona_texture_dimensions.map_or("native".into(),|(w,h)|format!("{w}x{h}")),
                                entry.png_color_type.map_or("none".to_string(),|x|x.to_string()),
                                entry.persona_rgba_normalized,
                                if entry.native{"native_persona"}else{"generated_persona_cfb8"},
                                if entry.native {""} else {" metadata_generated=true game_acceptance_unverified=true"}));
                            item_ids.insert(entry.id.clone());
                            reg.catalogs.insert(entry.id, entry.catalog);
                            for (path, asset) in entry.assets {
                                asset_ids.insert(path.clone());
                                reg.assets.insert(path, asset);
                            }
                            if entry.native {
                                reg.native_payloads += 1
                            } else {
                                reg.generated_persona_payloads += 1
                            }
                            reg.items.push(entry.item);
                        }
                        reg.accepted += 1;
                        reg.diagnostics.push(format!(
                            "CAPE_VALIDATED {label}: {name} capes={items} identity=stable payloads=registered"));
                    }
                    Err(err) => {
                        reg.rejected += 1;
                        reg.diagnostics
                            .push(format!("CAPE_REJECTED {label}: {err}"));
                    }
                }
            }
        }
        reg
    }

    fn read_pack(
        path: &Path,
        is_dir: bool,
        locale: &str,
    ) -> Result<(String, Vec<PreparedCape>), String> {
        let files = if is_dir {
            PackFiles::from_dir(path)?
        } else {
            PackFiles::from_zip(path)?
        };
        // Only the standard v1 layout is accepted; retired dev layouts are
        // rejected with a diagnostic instead of being silently shimmed.
        let folder_name = if is_dir {
            path.file_name().and_then(|value| value.to_str())
        } else {
            None
        };
        let standardized = standard_pack_manifest(&files, locale, folder_name)?;
        let pack: PackManifest = serde_json::from_slice(standardized.as_slice())
            .map_err(|e| format!("standard cape manifest invalid: {e}"))?;
        if !valid_uuid(&pack.id) {
            return Err("cape pack id must be a fixed UUID".into());
        }
        if !name_ok(&pack.name) {
            return Err("invalid package name".into());
        }
        if pack.capes.is_empty() || pack.capes.len() > MAX_CAPES_PER_PACK {
            return Err("cape pack must contain 1..64 capes".into());
        }
        let version = version_parts(&pack.version)?;
        let author = if pack.author.trim().is_empty() {
            "Player"
        } else {
            pack.author.trim()
        };
        if !name_ok(author) {
            return Err("invalid package author".into());
        }
        let mut keys = HashSet::new();
        let mut ids = HashSet::new();
        let mut resources = HashSet::new();
        let mut entries = Vec::with_capacity(pack.capes.len());
        for cape in pack.capes {
            if !stable_key(&cape.id) || !keys.insert(cape.id.clone()) {
                return Err("duplicate or invalid stable cape key".into());
            }
            let item_id = match cape.uuid {
                Some(id) if valid_uuid(&id) => id.to_ascii_lowercase(),
                Some(_) => return Err("cape uuid must be UUID".into()),
                None => uuid_from(
                    format!("{}\0{}", pack.id.to_ascii_lowercase(), cape.id).as_bytes(),
                    "item-v2",
                ),
            };
            if !ids.insert(item_id.clone()) {
                return Err("duplicate cape item UUID".into());
            }
            let author = cape
                .author
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or(author);
            let texture = cape.texture.as_deref();
            let persona = cape.persona_zip.as_deref();
            if texture.is_none() && persona.is_none() {
                return Err(format!("cape {} missing texture or persona_zip", cape.id));
            }
            let png = pack_file(&files, texture, MAX_IMAGE, false)?;
            let native = pack_file(&files, persona, MAX_NATIVE, false)?;
            if png.is_none() && native.is_none() {
                return Err(format!(
                    "cape {} texture or persona member missing",
                    cape.id
                ));
            }
            let icon = pack_file(&files, cape.icon.as_deref(), MAX_IMAGE, false)?;
            let prepared = Self::prepare_cape(
                item_id,
                cape.name,
                author,
                png,
                native,
                icon,
                &pack.version,
                version,
                cape.rarity.as_deref(),
                cape.start_date.as_deref(),
            )?;
            for (resource, _) in &prepared.assets {
                if !resources.insert(resource.clone()) {
                    return Err("duplicate cape resource UUID".into());
                }
            }
            entries.push(prepared);
        }
        Ok((pack.name, entries))
    }

    fn prepare_cape(
        item_id: String,
        name: String,
        author: &str,
        png: Option<Vec<u8>>,
        original: Option<Vec<u8>>,
        icon: Option<Vec<u8>>,
        version_text: &str,
        version: [u32; 3],
        rarity: Option<&str>,
        start_date: Option<&str>,
    ) -> Result<PreparedCape, String> {
        if !name_ok(&name) || !name_ok(author) {
            return Err("invalid cape name or author".into());
        }
        let rarity = normalized_rarity(rarity)?;
        if start_date.is_some_and(|date| !valid_start_date(date)) {
            return Err("startDate must be YYYY-MM-DDTHH:MM:SSZ".into());
        }
        let decoded = png.as_ref().map(|data| validate_png(data)).transpose()?;
        let dimensions = decoded.as_ref().map(|img| img.dimensions());
        let native = original.is_some();
        if !native {
            let (w, h) = dimensions.ok_or("PNG-only cape needs a texture")?;
            if w % 64 != 0 || h % 32 != 0 || w / 64 != h / 32 {
                return Err(format!(
                    "invalid cape UV dimensions {w}x{h}; expected 64x32, 128x64, 256x128, 512x256 or another 64x32 multiple"
                ));
            }
        }
        // Prefer the author-provided icon for the catalog thumbnail; fall back
        // to the synthesized mannequin preview. A Persona-only import can have
        // no separate cape.png. Keep every locally generated offer image URL
        // resolvable instead of emitting Cosmos-incompatible empty thumbnail URLs.
        let thumbnail = match icon {
            Some(bytes) => {
                // Reject broken icons instead of serving a blank catalog image.
                validate_png(&bytes)?;
                Some(bytes)
            }
            None => match decoded.as_ref() {
                Some(image) => Some(make_thumbnail(image)?),
                None => Some(make_thumbnail(&RgbaImage::from_pixel(
                    64,
                    32,
                    Rgba([76, 88, 105, 255]),
                ))?),
            },
        };
        let module_uuid = uuid_from(item_id.as_bytes(), "resource");
        let generated_pack_uuid = uuid_from(item_id.as_bytes(), "pack");
        let generated_version = if original.is_none() {
            Some(generated_pack_version(
                &name,
                author,
                png.as_deref().ok_or("missing cape PNG")?,
                version,
            ))
        } else {
            None
        };
        let identity = original
            .as_deref()
            .map(validate_native)
            .transpose()?
            .unwrap_or(NativeIdentity {
                uuid: generated_pack_uuid.clone(),
                version: generated_version
                    .map(version_string)
                    .unwrap_or_else(|| version_text.to_owned()),
            });
        let png_color_type = png.as_deref().and_then(|p| p.get(25)).copied();
        let mut converted_rgba = false;
        let mut persona_texture_dims = None;
        let persona = match original {
            // Keep native Persona bytes intact; PNG is only a preview here.
            Some(data) => {
                if is_binary_bundle(&data)? {
                    data
                } else {
                    super::persona_pack::bundle_pack(&data)?
                }
            }
            None => {
                let source = png.as_deref().ok_or("missing cape PNG")?;
                let decoded = decoded.as_ref().ok_or("missing decoded cape PNG")?;
                let (texture, converted, tex_dims) =
                    persona_texture_png(source, decoded)?;
                converted_rgba = converted;
                persona_texture_dims = Some(tex_dims);
                experimental_persona_png(
                    &name,
                    author,
                    &generated_pack_uuid,
                    &module_uuid,
                    &texture,
                    generated_version.unwrap_or(version),
                )?
            }
        };
        // Key download URLs to generated bytes, in addition to the pack
        // content-version change, to invalidate both HTTP and installed-pack
        // caches without changing the stable product ID or pack UUID.
        // Include the thumbnail in the fingerprint even for native Persona
        // resources where only the sidecar PNG was updated.
        let mut seed = Vec::with_capacity(item_id.len() + version_text.len() + 66);
        seed.extend_from_slice(item_id.as_bytes());
        seed.push(0);
        seed.extend_from_slice(version_text.as_bytes());
        seed.push(0);
        seed.extend_from_slice(&Sha256::digest(&persona));
        if let Some(ref image) = thumbnail {
            seed.extend_from_slice(&Sha256::digest(image));
        }
        let resource_id = uuid_from(&seed, "persona-content-v3");
        let url = format!(
            "https://{}/personabinary/{resource_id}.zip",
            super::persona::LOCAL_HOST
        );
        let thumbnail_path = format!("/thumbnails/{resource_id}.png");
        let thumbnail_url = if thumbnail.is_some() {
            format!("https://{}{}", super::persona::LOCAL_HOST, thumbnail_path)
        } else {
            String::new()
        };
        // This is the same offer schema as Cosmos CustomCapes and
        // overrides/capes_items.json. We do not copy Cosmos linksToInfo:
        // its PersonaCreatorPage_master_player_account!CustomCapes endpoint
        // is not enabled in Cosmetics and would create a broken navigation.
        let mut item = json!({
            "id":item_id,"contentType":"PersonaDurable","title":name,
            "creatorName":author,"pieceType":"persona_capes","ownership":"Purchased",
            "price":{"listPrice":0,"realmsInfo":{"inRealmsPlus":false},
                "currencyId":"ecd19d3c-7635-402c-a185-eb11cb6c6946",
                "virtualCurrencyType":"Minecoin"},
            "rarity":rarity,"packType":"Persona",
            "thumbnail":{"tag":"Thumbnail","type":"Thumbnail",
                "url":thumbnail_url,"urlWithResolution":thumbnail_url},
            "images":[{"tag":"Thumbnail","type":"Thumbnail",
                "url":thumbnail_url,"urlWithResolution":thumbnail_url}],
            "packIdentity":[{"type":"persona_piece","uuid":identity.uuid,"version":identity.version}]
        });
        if let Some(start_date) = start_date {
            item["startDate"] = Value::String(start_date.to_string());
        }
        let catalog = json!({
            "code":200,"status":"OK",
            "data":{"Item":{
                "Id":item_id,
                "Contents":[{"Id":resource_id,"Type":"personabinary","Url":url,
                    "MinClientVersion":"1.16.0","MaxClientVersion":"65535.65535.65535"}],
                "DisplayProperties":{"creatorName":author,"purchasable":true,
                    "packIdentity":[{"type":"persona_piece","uuid":identity.uuid,"version":identity.version}]}
            }}
        });
        let mut assets = vec![(
            format!("/personabinary/{resource_id}.zip"),
            LocalAsset {
                data: persona,
                kind: AssetType::PersonaZip,
            },
        )];
        if let Some(bytes) = thumbnail {
            assets.push((
                thumbnail_path,
                LocalAsset {
                    data: bytes,
                    kind: AssetType::Png,
                },
            ));
        }
        Ok(PreparedCape {
            id: item_id,
            item,
            catalog: serde_json::to_vec(&catalog).map_err(|e| e.to_string())?,
            assets,
            native,
            dimensions,
            png_color_type,
            persona_rgba_normalized: converted_rgba,
            persona_texture_dimensions: persona_texture_dims,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scanning_absent_capes_does_not_create_profile_directories() {
        let root = std::env::temp_dir().join(format!(
            "bloader-no-create-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let account = root.join("Users").join("1234567890123456789").join("games").join("com.mojang");
        assert!(!account.exists());
        let registry = Registry::scan(&[account.clone()], "en_US");
        assert_eq!(registry.discovered_packs, 0);
        assert_eq!(registry.discovered_roots, 0);
        assert!(!account.exists(), "cape scan must not construct a game profile");
        assert!(!root.exists(), "cape scan must not construct any ancestor");
    }
    #[test]
    fn standard_cape_folder_is_registered() {
        let root = std::env::temp_dir().join(format!(
            "bloader-cape-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let folder = root.join("capes").join("Example");
        fs::create_dir_all(folder.join("texts")).unwrap();
        fs::write(folder.join("manifest.json"),r#"{"format_version":1,"header":{"name":"Example","uuid":"11111111-2222-4333-8444-555555555555","version":[1,0,0]},"modules":[{"type":"cape_pack","uuid":"66666666-7777-4888-8999-aaaaaaaaaaaa","version":[1,0,0]}]}"#).unwrap();
        fs::write(folder.join("cape.json"),r#"{"localization_name":"example","capes":[{"localization_name":"red","texture":"cape.png"}]}"#).unwrap();
        fs::write(
            folder.join("texts/en_US.lang"),
            "cape.example.red=Red Cape\n",
        )
        .unwrap();
        fs::write(folder.join("cape.png"), dummy_png()).unwrap();
        let registry = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(registry.accepted, 1);
        fs::remove_dir_all(root).unwrap();
    }
    fn dummy_png() -> Vec<u8> {
        let mut img = RgbaImage::from_pixel(64, 32, Rgba([0, 0, 0, 0]));
        // UV back face should be visible in preview, not flattened gibberish.
        for y in 1..17 {
            for x in 1..11 {
                img.put_pixel(x, y, Rgba([220, 30, 30, 255]));
            }
        }
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn asset_urls_accept_query_without_widening_resource_lookup() {
        assert_eq!(
            canonical_asset_path("/personabinary/a.zip?version=5"),
            "/personabinary/a.zip"
        );
        assert_eq!(
            canonical_asset_path("/thumbnails/b.png?width=800&height=450"),
            "/thumbnails/b.png"
        );
        assert_eq!(
            canonical_asset_path("/personabinary/a.zip#section"),
            "/personabinary/a.zip"
        );
        assert_eq!(
            canonical_asset_path("/personabinary/a.zip"),
            "/personabinary/a.zip"
        );
    }
    /// 测试用的固定包身份：所有标准格式测试包共用同一 header/module UUID，
    /// 通过稳定键区分披风身份（与 read_pack 的 item-v2 派生一致）。
    const TEST_PACK_UUID: &str = "11111111-2222-4333-8444-555555555555";
    const TEST_MODULE_UUID: &str = "66666666-7777-4888-8999-000000000001";
    /// 向 ZIP 写入标准 v1 格式的 manifest.json / cape.json / texts/en_US.lang 成员。
    fn write_standard_members(
        zip: &mut ZipWriter<File>,
        key: &str,
        title: &str,
        extra_cape_json: &str,
        texture_member: Option<&str>,
    ) {
        let options = SimpleFileOptions::default();
        let manifest = format!(
            r#"{{"format_version":1,"header":{{"name":"{title}","uuid":"{TEST_PACK_UUID}","version":[1,0,0]}},"modules":[{{"type":"cape_pack","uuid":"{TEST_MODULE_UUID}","version":[1,0,0]}}]}}"#
        );
        let cape_json = format!(
            r#"{{"localization_name":"pack","capes":[{{"localization_name":"{key}","texture":{texture}{extra}}}]}}"#,
            texture = match texture_member {
                Some(path) => format!(r#""{path}""#),
                None => "null".to_string(),
            },
            extra = extra_cape_json
        );
        let lang = format!("capepack.pack={title}\ncape.pack.{key}={title}\n");
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.start_file("cape.json", options).unwrap();
        zip.write_all(cape_json.as_bytes()).unwrap();
        zip.start_file("texts/en_US.lang", options).unwrap();
        zip.write_all(lang.as_bytes()).unwrap();
    }
    fn pack_zip(root: &Path, name: &str, key: &str, title: &str) {
        pack_zip_extra(root, name, key, title, "")
    }
    fn pack_zip_extra(root: &Path, name: &str, key: &str, title: &str, extra_cape_json: &str) {
        fs::create_dir_all(root.join("capes")).unwrap();
        let file = File::create(root.join("capes").join(name)).unwrap();
        let mut zip = ZipWriter::new(file);
        write_standard_members(&mut zip, key, title, extra_cape_json, Some("cape.png"));
        let options = SimpleFileOptions::default();
        zip.start_file("cape.png", options).unwrap();
        zip.write_all(&dummy_png()).unwrap();
        zip.finish().unwrap();
    }
    // Build a valid indexed PNG (color type 3) without new dependencies.
    // This exercises the exact failure category in the 1280x640 user ZIP.
    fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc = 0xffff_ffffu32;
        for &byte in kind.iter().chain(data.iter()) {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = (crc >> 1) ^ (if crc & 1 == 1 { 0xedb8_8320 } else { 0 });
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
    }
    fn palette_cape_png(scale: u32) -> Vec<u8> {
        use flate2::{Compression, write::ZlibEncoder};
        let (w, h) = (64 * scale, 32 * scale);
        let mut rows = Vec::with_capacity(((w + 1) * h) as usize);
        for y in 0..h {
            rows.push(0); // no PNG row filter
            for x in 0..w {
                rows.push(u8::from(
                    x >= scale && x < 11 * scale && y >= scale && y < 17 * scale,
                ));
            }
        }
        let mut z = ZlibEncoder::new(Vec::new(), Compression::fast());
        z.write_all(&rows).unwrap();
        let compressed = z.finish().unwrap();
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 3, 0, 0, 0]); // palette type 3, 8-bit
        png_chunk(&mut out, b"IHDR", &ihdr);
        png_chunk(&mut out, b"PLTE", &[19, 25, 30, 220, 30, 40]);
        png_chunk(&mut out, b"IDAT", &compressed);
        png_chunk(&mut out, b"IEND", &[]);
        out
    }
    #[test]
    fn indexed_1280x640_cape_is_transcoded_to_rgba_for_persona() {
        let original = palette_cape_png(20);
        assert_eq!(original[25], 3);
        let decoded = validate_png(&original).unwrap();
        assert_eq!(decoded.dimensions(), (1280, 640));
        // The raw RGBA conversion keeps its original dimensions, but the
        // game's generated Persona receives the fallback 64x32 atlas.
        let (rgba, changed) = persona_rgba_png(&original, &decoded).unwrap();
        assert!(changed);
        assert_eq!(rgba[24], 8);
        assert_eq!(rgba[25], 6);
        assert_eq!(validate_png(&rgba).unwrap(), decoded);
        let (compatible, converted, dims) =
            persona_texture_png(&original, &decoded).unwrap();
        assert!(converted);
        assert_eq!(dims, (64, 32));
        assert_eq!(compatible[25], 6);
        let reduced = validate_png(&compatible).unwrap();
        assert_eq!(reduced.dimensions(), (64, 32));
        assert_eq!(reduced.get_pixel(4, 5), &Rgba([220, 30, 40, 255]));
        let small = persona_texture_png(
            &dummy_png(),
            &validate_png(&dummy_png()).unwrap(),
        )
        .unwrap();
        assert!(
            !small.1,
            "already compatible RGBA8 must stay byte-identical"
        );
        let id = "11111111-2222-4333-8444-555555555555".to_string();
        let cape = Registry::prepare_cape(
            id.clone(),
            "HD".to_string(),
            "Player",
            Some(original.clone()),
            None,
            None,
            "1.0.0",
            [1, 0, 0],
            None,
            None,
        )
        .unwrap();
        assert_eq!(cape.dimensions, Some((1280, 640)));
        assert_eq!(cape.persona_texture_dimensions, Some((64, 32)));
        assert_eq!(cape.png_color_type, Some(3));
        assert!(cape.persona_rgba_normalized);
        let module = uuid_from(id.as_bytes(), "resource");
        let pack = uuid_from(id.as_bytes(), "pack");
        let version = generated_pack_version("HD", "Author", &original, [1, 0, 0]);
        let expected =
            crate::cosmetics::persona_pack::generate("HD", "Author", &pack, &module, &compatible, version)
                .unwrap();
        let served = cape
            .assets
            .iter()
            .find(|(path, _)| path.starts_with("/personabinary/"))
            .unwrap();
        assert_eq!(
            served.1.data, expected,
            "downloaded Persona bundle must contain RGBA-converted texture"
        );
        let thumb = cape
            .assets
            .iter()
            .find(|(path, _)| path.starts_with("/thumbnails/"))
            .unwrap();
        let preview = validate_png(&thumb.1.data).unwrap();
        assert_eq!(preview.dimensions(), (64, 64));
        assert_eq!(preview.get_pixel(31, 33), &Rgba([220, 30, 40, 255]));
        assert_ne!(
            preview.get_pixel(31, 8),
            &Rgba([220, 30, 40, 255]),
            "neutral mannequin head stays above the back cape"
        );
    }
    #[test]
    fn high_resolution_rgba8_also_uses_standard_persona_texture() {
        // RGBA8 alone does not guarantee that Minecraft's built-in Persona
        // renderer will accept a 20x atlas. Test both input representations.
        let mut source = RgbaImage::from_pixel(1280, 640, Rgba([0, 0, 0, 0]));
        for y in 20..340 {
            for x in 20..220 {
                source.put_pixel(x, y, Rgba([21, 151, 230, 255]));
            }
        }
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut encoded, ImageFormat::Png)
            .unwrap();
        let png = encoded.into_inner();
        assert_eq!(png[25], 6);
        let decoded = validate_png(&png).unwrap();
        assert_eq!(decoded.dimensions(), (1280, 640));
        let (payload, transcoded, _) =
            persona_texture_png(&png, &decoded).unwrap();
        assert!(
            transcoded,
            "20x RGBA8 still requires Persona 64x32 compatibility"
        );
        assert_eq!(payload[25], 6);
        let tex = validate_png(&payload).unwrap();
        assert_eq!(tex.dimensions(), (64, 32));
        assert_eq!(tex.get_pixel(4, 4), &Rgba([21, 151, 230, 255]));
        assert_eq!(tex.get_pixel(13, 4), &Rgba([0, 0, 0, 0]));
    }
    #[test]
    fn uv_compat_downsample_keeps_separate_faces_and_alpha() {
        let mut source = RgbaImage::from_pixel(128, 64, Rgba([0, 0, 0, 0]));
        for y in 2..34 {
            for x in 2..22 {
                source.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
            for x in 24..44 {
                source.put_pixel(x, y, Rgba([0, 255, 0, 255]));
            }
        }
        let out = cape_compat_rgba(&source);
        assert_eq!(out.dimensions(), (64, 32));
        assert_eq!(out.get_pixel(4, 5), &Rgba([255, 0, 0, 255]));
        assert_eq!(out.get_pixel(15, 5), &Rgba([0, 255, 0, 255]));
        assert_eq!(out.get_pixel(22, 5), &Rgba([0, 0, 0, 0]));
        assert_eq!(out.get_pixel(0, 0), &Rgba([0, 0, 0, 0]));
        assert_eq!(
            out.get_pixel(11, 5),
            &Rgba([0, 0, 0, 0]),
            "outer face and lining must not bleed across UV boundary"
        );
    }
    #[test]
    fn cape_preview_uses_outer_back_uv_not_inner_front() {
        let mut image = RgbaImage::from_pixel(64, 32, Rgba([0, 0, 0, 0]));
        for y in 1..17 {
            for x in 1..11 {
                image.put_pixel(x, y, Rgba([0, 255, 0, 255]));
            }
            for x in 12..22 {
                image.put_pixel(x, y, Rgba([255, 30, 20, 255]));
            }
        }
        let preview = validate_png(&make_thumbnail(&image).unwrap()).unwrap();
        assert_eq!(preview.get_pixel(31, 33), &Rgba([0, 255, 0, 255]));
        assert_ne!(preview.get_pixel(31, 33), &Rgba([255, 30, 20, 255]));
    }
    #[test]
    fn one_zip_is_one_cape_without_catalog_files() {
        let root = std::env::temp_dir().join(format!("cosmetics_pack_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        pack_zip(&root, "MyCape.zip", "red", "Red Cape");
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(reg.items.len(), 1, "{:?}", reg.diagnostics);
        assert_eq!(
            (reg.discovered_packs, reg.accepted, reg.rejected),
            (1, 1, 0)
        );
        assert_eq!(reg.generated_persona_payloads, 1);
        assert_eq!(reg.items[0]["title"], "Red Cape");
        assert_eq!(reg.items[0]["creatorName"], "Player");
        assert_eq!(reg.items[0]["ownership"], "Purchased");
        let id = reg.items[0]["id"].as_str().unwrap();
        let catalog: Value = serde_json::from_slice(reg.catalogs.get(id).unwrap()).unwrap();
        assert_eq!(catalog["data"]["Item"]["Id"], id);
        let path = catalog["data"]["Item"]["Contents"][0]["Url"]
            .as_str()
            .unwrap()
            .strip_prefix("https://cosmetics.invalid")
            .unwrap();
        assert_eq!(reg.assets.get(path).unwrap().kind, AssetType::PersonaZip);
        let img = reg
            .assets
            .values()
            .find(|a| a.kind == AssetType::Png)
            .unwrap();
        let preview = validate_png(&img.data).unwrap();
        assert_eq!(preview.dimensions(), (64, 64));
        assert_eq!(preview.get_pixel(32, 32), &Rgba([220, 30, 30, 255]));
        let native = &reg.assets.get(path).unwrap().data;
        let mut outer = ZipArchive::new(Cursor::new(native)).unwrap();
        let mut pack = Vec::new();
        outer
            .by_name("ppack0.zip")
            .unwrap()
            .read_to_end(&mut pack)
            .unwrap();
        let mut zip = ZipArchive::new(Cursor::new(pack)).unwrap();
        assert!(
            zip.file_names()
                .flatten()
                .any(|name| name.ends_with("_cape.png"))
        );
        let manifest: Value =
            serde_json::from_reader(zip.by_name("manifest.json").unwrap()).unwrap();
        assert_eq!(
            manifest["header"]["uuid"],
            reg.items[0]["packIdentity"][0]["uuid"]
        );
        assert_ne!(manifest["modules"][0]["uuid"], manifest["header"]["uuid"]);
        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn mccapes_archive_is_scanned_as_cape_pack() {
        let root = std::env::temp_dir().join(format!("cosmetics_mccapes_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        pack_zip(&root, "Example.mccapes", "example", "Example Cape");
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(
            (reg.discovered_packs, reg.accepted, reg.rejected),
            (1, 1, 0),
            "{:?}",
            reg.diagnostics
        );
        assert_eq!(reg.items[0]["title"], "Example Cape");
        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn generated_persona_uses_cosmos_bundle_and_encrypted_index() {
        let data = experimental_persona_png(
            "Local Test",
            "Test Artist",
            "11111111-2222-4333-8444-555555555555",
            "66666666-2222-4333-8444-555555555555",
            &dummy_png(),
            [1, 0, 0],
        )
        .unwrap();
        let mut outer = ZipArchive::new(Cursor::new(data)).unwrap();
        assert_eq!(outer.len(), 1);
        let mut nested = Vec::new();
        outer
            .by_name("ppack0.zip")
            .unwrap()
            .read_to_end(&mut nested)
            .unwrap();
        let mut archive = ZipArchive::new(Cursor::new(nested)).unwrap();
        let manifest: Value =
            serde_json::from_reader(archive.by_name("manifest.json").unwrap()).unwrap();
        assert_eq!(manifest["format_version"], 1);
        assert!(
            archive
                .file_names()
                .flatten()
                .any(|path| path.ends_with("_cape.meta.json"))
        );
        let mut index = Vec::new();
        archive
            .by_name("contents.json")
            .unwrap()
            .read_to_end(&mut index)
            .unwrap();
        assert_eq!(&index[4..8], &2614082044u32.to_le_bytes());
        assert!(archive.by_name("signatures.json").is_ok());
    }
    #[test]
    fn local_item_metadata_keeps_cosmos_thumbnail_fields() {
        let root = std::env::temp_dir().join(format!("cosmetics_thumbs_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        pack_zip(&root, "cape.zip", "cape", "Local Cape");
        let reg = Registry::scan(&[root.clone()], "en_US");
        let item = &reg.items[0];
        assert_eq!(item["images"][0]["url"], item["thumbnail"]["url"]);
        assert_eq!(
            item["images"][0]["urlWithResolution"],
            item["thumbnail"]["urlWithResolution"]
        );
        assert_eq!(item["packIdentity"][0]["type"], "persona_piece");
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn cosmos_custom_cape_offer_and_catalog_have_matching_identities() {
        let root =
            std::env::temp_dir().join(format!("cosmetics_cosmos_shape_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        pack_zip_extra(
            &root,
            "cosmos.zip",
            "sample",
            "Sample Cape",
            r#","rarity":"legendary","startDate":"2026-10-17T10:20:30Z""#,
        );
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(reg.items.len(), 1, "{:?}", reg.diagnostics);
        let item = &reg.items[0];
        for field in [
            "id",
            "contentType",
            "title",
            "creatorName",
            "thumbnail",
            "price",
            "pieceType",
            "rarity",
            "ownership",
            "packType",
            "packIdentity",
            "images",
            "startDate",
        ] {
            assert!(
                item.get(field).is_some(),
                "missing Cosmos offer field {field}"
            );
        }
        assert_eq!(item["contentType"], "PersonaDurable");
        assert_eq!(item["pieceType"], "persona_capes");
        assert_eq!(item["rarity"], "legendary");
        assert_eq!(item["startDate"], "2026-10-17T10:20:30Z");
        assert_eq!(item["thumbnail"]["url"], item["images"][0]["url"]);
        assert!(!item["thumbnail"]["url"].as_str().unwrap().is_empty());
        assert!(
            item.get("linksToInfo").is_none(),
            "unhandled Cosmos detail page must not be linked"
        );
        let id = item["id"].as_str().unwrap();
        let catalog: Value = serde_json::from_slice(reg.catalogs.get(id).unwrap()).unwrap();
        let published = &catalog["data"]["Item"];
        assert_eq!(catalog["code"], 200);
        assert_eq!(published["Id"], id);
        assert_eq!(
            published["DisplayProperties"]["packIdentity"],
            item["packIdentity"]
        );
        assert_eq!(
            published["DisplayProperties"]["creatorName"],
            item["creatorName"]
        );
        assert_eq!(published["Contents"][0]["Type"], "personabinary");
        assert_eq!(published["Contents"][0]["MinClientVersion"], "1.16.0");
        assert_eq!(
            published["Contents"][0]["MaxClientVersion"],
            "65535.65535.65535"
        );
        let url = published["Contents"][0]["Url"].as_str().unwrap();
        assert!(url.starts_with("https://cosmetics.invalid/personabinary/"));
        assert!(
            reg.assets
                .contains_key(url.strip_prefix("https://cosmetics.invalid").unwrap())
        );
        let thumbnail_url = item["thumbnail"]["url"].as_str().unwrap();
        assert!(
            reg.assets.contains_key(
                thumbnail_url
                    .strip_prefix("https://cosmetics.invalid")
                    .unwrap()
            )
        );
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn invalid_cosmos_offer_metadata_fails_before_registration() {
        let root = std::env::temp_dir().join(format!("cosmetics_bad_offer_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        pack_zip_extra(
            &root,
            "invalid.zip",
            "invalid",
            "Invalid Cape",
            r#","rarity":"invalid""#,
        );
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(reg.rejected, 1);
        assert!(reg.items.is_empty());
        assert!(
            reg.diagnostics
                .iter()
                .any(|line| line.contains("unsupported cape rarity"))
        );
        pack_zip_extra(
            &root,
            "invalid.zip",
            "invalid",
            "Invalid Cape",
            r#","startDate":"untrusted""#,
        );
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(reg.rejected, 1);
        assert!(reg.items.is_empty());
        assert!(
            reg.diagnostics
                .iter()
                .any(|line| line.contains("startDate must be"))
        );
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn original_native_module_identity_is_used_in_catalog_and_wardrobe() {
        let root = std::env::temp_dir().join(format!("cosmetics_native_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("capes")).unwrap();
        let uuid = "a6c559a1-22b3-44c5-88d6-6ff777888999";
        let module_uuid = "12345678-22b3-44c5-88d6-6ff777888999";
        let mut native_zip = ZipWriter::new(Cursor::new(Vec::new()));
        native_zip
            .start_file("manifest.json", SimpleFileOptions::default())
            .unwrap();
        native_zip.write_all(format!(
            r#"{{"format_version":1,"header":{{"uuid":"{uuid}","version":[2,3,4]}},"modules":[{{"type":"persona_piece","uuid":"{module_uuid}","version":[9,9,9]}}]}}"#
        ).as_bytes()).unwrap();
        let native = native_zip.finish().unwrap().into_inner();
        let mut outer = ZipWriter::new(File::create(root.join("capes/native.zip")).unwrap());
        write_standard_members(
            &mut outer,
            "native",
            "Native Cape",
            r#","persona_zip":"persona.zip""#,
            None,
        );
        outer
            .start_file("persona.zip", SimpleFileOptions::default())
            .unwrap();
        outer.write_all(&native).unwrap();
        outer.finish().unwrap();
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(reg.accepted, 1, "{:?}", reg.diagnostics);
        assert_eq!(reg.native_payloads, 1);
        assert_eq!(reg.items[0]["packIdentity"][0]["uuid"], uuid);
        assert_eq!(reg.items[0]["packIdentity"][0]["version"], "2.3.4");
        let id = reg.items[0]["id"].as_str().unwrap();
        let product: Value = serde_json::from_slice(reg.catalogs.get(id).unwrap()).unwrap();
        assert_eq!(
            product["data"]["Item"]["DisplayProperties"]["packIdentity"][0]["uuid"],
            uuid
        );
        assert_eq!(
            product["data"]["Item"]["DisplayProperties"]["packIdentity"][0]["version"],
            "2.3.4"
        );
        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn malformed_png_is_rejected_instead_of_registering_a_false_positive() {
        assert!(validate_png(b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR").is_err());
    }
    #[test]
    fn cape_identity_is_stable_across_rescans() {
        let root = std::env::temp_dir().join(format!("cosmetics_unnamed_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        pack_zip(&root, "blue.zip", "blue", "Blue Cape");
        let a = Registry::scan(&[root.clone()], "en_US");
        let b = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(a.items[0]["id"], b.items[0]["id"]);
        assert_eq!(a.items[0]["title"], "Blue Cape");
        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn corrupt_zip_is_rejected_without_panic() {
        let root = std::env::temp_dir().join(format!("cosmetics_badpack_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("capes")).unwrap();
        fs::write(root.join("capes/bad.zip"), b"not a zip").unwrap();
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(reg.items.len(), 0);
        assert_eq!(
            (reg.discovered_packs, reg.accepted, reg.rejected),
            (1, 0, 1)
        );
        // CAPE_ROOT 摘要 + CAPE_REJECTED 一条；不含其他噪声。
        assert_eq!(
            reg.diagnostics
                .iter()
                .filter(|x| x.contains("CAPE_REJECTED"))
                .count(),
            1
        );
        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn scan_summary_distinguishes_valid_and_failed_zips() {
        let root = std::env::temp_dir().join(format!("cosmetics_mix_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        pack_zip(&root, "working.zip", "working", "Working Cape");
        fs::write(root.join("capes/broken.zip"), b"not-zip").unwrap();
        let registry = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(registry.discovered_packs, 2);
        assert_eq!(registry.accepted, 1);
        assert_eq!(registry.rejected, 1);
        assert_eq!(registry.asset_count(), 2);
        assert_eq!(registry.generated_persona_payloads, 1);
        assert_eq!(registry.native_payloads, 0);
        let _ = fs::remove_dir_all(&root);
    }
    fn write_multi(root: &Path, filename: &str, version: &str, duplicate: bool) {
        fs::create_dir_all(root.join("capes")).unwrap();
        let author_version: Vec<Value> = version
            .split('.')
            .map(|part| json!(part.parse::<u64>().unwrap_or(0)))
            .collect();
        let capes = if duplicate {
            json!([
                {"localization_name":"red","texture":"textures/red.png"},
                {"localization_name":"red","texture":"textures/blue.png"}
            ])
        } else {
            json!([
                {"localization_name":"red","texture":"textures/red.png"},
                {"localization_name":"blue","texture":"textures/blue.png"}
            ])
        };
        let manifest = json!({
            "format_version":1,
            "header":{"name":"My Cape Collection","uuid":TEST_PACK_UUID,"version":author_version},
            "modules":[{"type":"cape_pack","uuid":TEST_MODULE_UUID,"version":[1,0,0]}]
        });
        let sheet = json!({
            "serialize_name":"My Cape Collection","localization_name":"my_cape_collection",
            "capes":capes
        });
        let mut writer = ZipWriter::new(File::create(root.join("capes").join(filename)).unwrap());
        let opts = SimpleFileOptions::default();
        writer.start_file("manifest.json", opts).unwrap();
        writer
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        writer.start_file("cape.json", opts).unwrap();
        writer
            .write_all(&serde_json::to_vec(&sheet).unwrap())
            .unwrap();
        writer.start_file("texts/en_US.lang", opts).unwrap();
        writer.write_all(b"capepack.my_cape_collection=My Cape Collection\ncape.my_cape_collection.red=Red Cape\ncape.my_cape_collection.blue=Blue Cape\n").unwrap();
        for path in ["textures/red.png", "textures/blue.png"] {
            writer.start_file(path, opts).unwrap();
            writer.write_all(&dummy_png()).unwrap();
        }
        writer.finish().unwrap();
    }
    #[test]
    fn multi_cape_zip_registers_all_items_and_stable_ids_across_boots_and_rename() {
        let root = std::env::temp_dir().join(format!("cosmetics_multicape_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write_multi(&root, "old-name.zip", "1.0.0", false);
        let a = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(a.discovered_packs, 1);
        assert_eq!(a.accepted, 1);
        assert_eq!(a.items.len(), 2);
        assert_eq!(a.assets.len(), 4);
        assert_eq!(a.generated_persona_payloads, 2);
        assert_eq!(a.items[0]["title"], "Red Cape");
        assert_eq!(a.items[1]["title"], "Blue Cape");
        fs::rename(
            root.join("capes/old-name.zip"),
            root.join("capes/renamed.zip"),
        )
        .unwrap();
        let b = Registry::scan(&[root.clone()], "en_US");
        let c = Registry::scan(&[root.clone()], "en_US");
        for i in 0..2 {
            assert_eq!(a.items[i]["id"], b.items[i]["id"]);
            assert_eq!(b.items[i]["id"], c.items[i]["id"]);
            assert_eq!(a.items[i]["packIdentity"], b.items[i]["packIdentity"]);
            let key = a.items[i]["id"].as_str().unwrap();
            assert_eq!(a.catalogs.get(key), b.catalogs.get(key));
        }
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn generated_persona_version_is_content_addressed_and_stable() {
        let image = dummy_png();
        let v = generated_pack_version("Example", "Artist", &image, [1, 0, 0]);
        assert_ne!(
            v,
            [1, 0, 0],
            "generated version must not hit obsolete cache"
        );
        assert_eq!(v, generated_pack_version("Example", "Artist", &image, [1, 0, 0]));
        assert_ne!(v, generated_pack_version("Renamed", "Artist", &image, [1, 0, 0]));
        assert_ne!(v, generated_pack_version("Example", "Other Artist", &image, [1, 0, 0]));
        assert_ne!(v, generated_pack_version("Example", "Artist", &image, [1, 0, 1]));
        assert_eq!(version_string(v).split('.').count(), 3);
        let id = "11111111-2222-4333-8444-555555555555".to_string();
        let make = |data: Vec<u8>| {
            Registry::prepare_cape(
                id.clone(),
                "Example".to_owned(),
                "Artist",
                Some(data),
                None,
                None,
                "1.0.0",
                [1, 0, 0],
                None,
                None,
            )
            .unwrap()
        };
        let registered = make(image);
        let catalog: Value = serde_json::from_slice(&registered.catalog).unwrap();
        assert_eq!(
            catalog["data"]["Item"]["DisplayProperties"]["packIdentity"],
            registered.item["packIdentity"]
        );
        let outer = registered
            .assets
            .iter()
            .find(|(path, _)| path.starts_with("/personabinary/"))
            .unwrap();
        let mut package = ZipArchive::new(Cursor::new(&outer.1.data)).unwrap();
        let mut bytes = Vec::new();
        package
            .by_name("ppack0.zip")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        let mut inner = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let manifest: Value =
            serde_json::from_reader(inner.by_name("manifest.json").unwrap()).unwrap();
        assert_eq!(
            manifest["header"]["uuid"],
            registered.item["packIdentity"][0]["uuid"]
        );
        let expected = version_string(v);
        assert_eq!(registered.item["packIdentity"][0]["version"], expected);
        assert_eq!(manifest["header"]["version"], json!(v));
        assert_eq!(manifest["modules"][0]["version"], json!(v));
        assert_eq!(manifest["metadata"]["authors"], json!(["Artist"]));
    }
    #[test]
    fn resource_urls_change_with_generated_persona_content_not_item_identity() {
        let id = "11111111-2222-4333-8444-555555555555".to_owned();
        let png = dummy_png();
        let mut modified = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(64, 32, Rgba([4, 5, 6, 255])))
            .write_to(&mut modified, ImageFormat::Png)
            .unwrap();
        let make = |image: Vec<u8>| {
            Registry::prepare_cape(
                id.clone(),
                "Local Cape".to_owned(),
                "Player",
                Some(image),
                None,
                None,
                "1.0.0",
                [1, 0, 0],
                None,
                None,
            )
            .unwrap()
        };
        let a = make(png.clone());
        let b = make(modified.into_inner());
        let again = make(png);
        assert_eq!(a.id, b.id);
        assert_eq!(
            a.item["packIdentity"][0]["uuid"],
            b.item["packIdentity"][0]["uuid"]
        );
        assert_ne!(
            a.item["packIdentity"][0]["version"], b.item["packIdentity"][0]["version"],
            "changed PNG must invalidate the Persona package version"
        );
        assert_eq!(a.item["packIdentity"], again.item["packIdentity"]);
        let cat_a: Value = serde_json::from_slice(&a.catalog).unwrap();
        let cat_b: Value = serde_json::from_slice(&b.catalog).unwrap();
        let cat_again: Value = serde_json::from_slice(&again.catalog).unwrap();
        assert_ne!(
            cat_a["data"]["Item"]["Contents"][0]["Id"],
            cat_b["data"]["Item"]["Contents"][0]["Id"]
        );
        assert_ne!(
            cat_a["data"]["Item"]["Contents"][0]["Url"],
            cat_b["data"]["Item"]["Contents"][0]["Url"]
        );
        assert_ne!(a.item["thumbnail"]["url"], b.item["thumbnail"]["url"]);
        assert_eq!(
            cat_a["data"]["Item"]["Contents"][0]["Url"],
            cat_again["data"]["Item"]["Contents"][0]["Url"]
        );
        assert_eq!(a.item["thumbnail"]["url"], again.item["thumbnail"]["url"]);
    }
    #[test]
    fn explicit_pack_version_changes_asset_urls_not_item_identity() {
        let root =
            std::env::temp_dir().join(format!("cosmetics_packversion_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write_multi(&root, "collection.zip", "1.0.0", false);
        let a = Registry::scan(&[root.clone()], "en_US");
        write_multi(&root, "collection.zip", "1.0.1", false);
        let b = Registry::scan(&[root.clone()], "en_US");
        for i in 0..2 {
            assert_eq!(a.items[i]["id"], b.items[i]["id"]);
            assert_eq!(
                a.items[i]["packIdentity"][0]["uuid"],
                b.items[i]["packIdentity"][0]["uuid"]
            );
            assert_ne!(
                a.items[i]["packIdentity"][0]["version"], b.items[i]["packIdentity"][0]["version"],
                "author pack version changes must invalidate installed Persona"
            );
            let v = a.items[i]["packIdentity"][0]["version"].as_str().unwrap();
            assert!(v.starts_with("1."));
            let updated = b.items[i]["packIdentity"][0]["version"].as_str().unwrap();
            assert!(updated.starts_with("1."));
            let id = a.items[i]["id"].as_str().unwrap();
            let first: Value = serde_json::from_slice(a.catalogs.get(id).unwrap()).unwrap();
            let next: Value = serde_json::from_slice(b.catalogs.get(id).unwrap()).unwrap();
            assert_ne!(
                first["data"]["Item"]["Contents"][0]["Url"],
                next["data"]["Item"]["Contents"][0]["Url"]
            );
        }
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn invalid_multi_cape_pack_fails_atomically_without_partial_registration() {
        let root = std::env::temp_dir().join(format!("cosmetics_duplicate_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write_multi(&root, "duplicate.zip", "1.0.0", true);
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(reg.discovered_packs, 1);
        assert_eq!(reg.rejected, 1);
        assert!(reg.items.is_empty());
        assert!(reg.assets.is_empty());
        assert!(reg.catalogs.is_empty());
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn multi_cape_pack_rejects_traversal_and_invalid_versions() {
        assert!(!safe_entry("../textures/red.png"));
        assert!(!safe_entry("textures\\red.png"));
        assert!(!safe_entry("/outside.png"));
        assert!(safe_entry("textures/red.png"));
        assert!(safe_entry("textures/我的披风.png"));
        assert!(safe_entry("textures/Red Cape.png"));
        assert_eq!(version_parts("1.2.3").unwrap(), [1, 2, 3]);
        assert!(version_parts("1.2").is_err());
        assert!(version_parts("1.2.999999").is_err());
    }
    #[test]
    fn high_resolution_source_uv_is_preserved_and_persona_uses_64x32_compat() {
        for (scale, w, h) in [(2, 128, 64), (4, 256, 128), (8, 512, 256), (20, 1280, 640)] {
            let root = std::env::temp_dir().join(format!(
                "cosmetics_hires_{}_{}",
                std::process::id(),
                scale
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("capes")).unwrap();
            let mut image = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
            for y in scale..17 * scale {
                for x in scale..11 * scale {
                    image.put_pixel(x, y, Rgba([200, 50, 80, 255]));
                }
            }
            let mut encoded = Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image)
                .write_to(&mut encoded, ImageFormat::Png)
                .unwrap();
            let mut outer = ZipWriter::new(File::create(root.join("capes/hires.zip")).unwrap());
            write_standard_members(&mut outer, "hires", "Hi-Res Cape", "", Some("cape.png"));
            outer
                .start_file("cape.png", SimpleFileOptions::default())
                .unwrap();
            outer.write_all(encoded.get_ref()).unwrap();
            outer.finish().unwrap();
            let reg = Registry::scan(&[root.clone()], "en_US");
            assert_eq!(reg.accepted, 1, "{:?}", reg.diagnostics);
            assert_eq!(reg.generated_persona_payloads, 1);
            assert!(
                reg.diagnostics
                    .iter()
                    .any(|line| line.contains("persona_texture=64x32"))
            );
            let data = reg
                .assets
                .values()
                .find(|v| v.kind == AssetType::PersonaZip)
                .unwrap();
            let mut outer = ZipArchive::new(Cursor::new(&data.data)).unwrap();
            let mut nested = Vec::new();
            outer
                .by_name("ppack0.zip")
                .unwrap()
                .read_to_end(&mut nested)
                .unwrap();
            let mut inner = ZipArchive::new(Cursor::new(nested)).unwrap();
            let texture = inner
                .file_names()
                .flatten()
                .find(|path| path.ends_with("_cape.png"))
                .unwrap()
                .to_string();
            let mut encrypted = Vec::new();
            inner
                .by_name(&texture)
                .unwrap()
                .read_to_end(&mut encrypted)
                .unwrap();
            assert_ne!(
                encrypted.as_slice(),
                encoded.get_ref().as_slice(),
                "PNG must be encrypted"
            );
            assert_eq!(encrypted.len() % 16, 0);
            let decoded = validate_png(encoded.get_ref()).unwrap();
            assert_eq!(decoded.dimensions(), (w, h));
            assert!(
                reg.diagnostics
                    .iter()
                    .any(|line| line.contains(&format!("texture={w}x{h}")))
            );
            let _ = fs::remove_dir_all(root);
        }
    }
    #[test]
    fn non_cape_aspect_ratio_is_rejected_with_diagnostic() {
        let root = std::env::temp_dir().join(format!("cosmetics_wrong_uv_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("capes")).unwrap();
        let image = RgbaImage::from_pixel(128, 128, Rgba([255, 20, 20, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        let mut zip = ZipWriter::new(File::create(root.join("capes/wrong.zip")).unwrap());
        write_standard_members(&mut zip, "wrong", "Wrong UV Cape", "", Some("cape.png"));
        zip.start_file("cape.png", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes.get_ref()).unwrap();
        zip.finish().unwrap();
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(reg.rejected, 1);
        assert_eq!(reg.items.len(), 0);
        assert!(
            reg.diagnostics
                .iter()
                .any(|x| x.contains("invalid cape UV dimensions 128x128"))
        );
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn cape_png_requires_png_signature() {
        assert!(validate_png(b"GIF89a").is_err());
    }

    // ---- BLoader 移植扩展：解包文件夹包与多根扫描 ----

    /// 把一个标准 v1 单披风包写成解包文件夹形式。
    fn write_folder_pack(root: &Path, folder: &str, key: &str, title: &str) {
        let dir = root.join("capes").join(folder);
        fs::create_dir_all(dir.join("texts")).unwrap();
        let manifest = format!(
            r#"{{"format_version":1,"header":{{"name":"{title}","uuid":"{TEST_PACK_UUID}","version":[1,0,0]}},"modules":[{{"type":"cape_pack","uuid":"{TEST_MODULE_UUID}","version":[1,0,0]}}]}}"#
        );
        let cape_json = format!(
            r#"{{"localization_name":"pack","capes":[{{"localization_name":"{key}","texture":"cape.png"}}]}}"#
        );
        fs::write(dir.join("manifest.json"), manifest).unwrap();
        fs::write(dir.join("cape.json"), cape_json).unwrap();
        fs::write(
            dir.join("texts/en_US.lang"),
            format!("capepack.pack={title}\ncape.pack.{key}={title}\n"),
        )
        .unwrap();
        fs::write(dir.join("cape.png"), dummy_png()).unwrap();
    }
    /// A bare single-cape folder (cape.json + texture, no manifest.json) is a
    /// supported layout; its pack UUID derives from the folder name so the
    /// identity stays stable.
    #[test]
    fn bare_single_cape_folder_registers_without_manifest() {
        let root = std::env::temp_dir().join(format!("cosmetics_barecape_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("capes").join("共享披风");
        fs::create_dir_all(&dir).unwrap();
        // Descriptor-only layout, exactly what a hand-made folder looks like:
        // no manifest, no cape list, just a name and the texture next to it.
        fs::write(
            dir.join("cape.json"),
            r#"{"name":"共享披风","author":"Single Artist"}"#,
        )
        .unwrap();
        fs::write(dir.join("cape.png"), dummy_png()).unwrap();

        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(
            (reg.discovered_packs, reg.accepted, reg.rejected),
            (1, 1, 0),
            "{:?}",
            reg.diagnostics
        );
        assert_eq!(reg.items.len(), 1);
        assert_eq!(reg.items[0]["creatorName"], "Single Artist");
        let id = reg.items[0]["id"].as_str().unwrap();
        let catalog: Value = serde_json::from_slice(reg.catalogs.get(id).unwrap()).unwrap();
        assert_eq!(catalog["data"]["Item"]["DisplayProperties"]["creatorName"], "Single Artist");

        // Stable identity: scanning again yields the same item UUID.
        let again = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(again.items[0]["id"], reg.items[0]["id"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unpacked_folder_pack_registers_like_zip() {
        let root = std::env::temp_dir().join(format!("cosmetics_dirpack_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write_folder_pack(&root, "我的披风1", "my_cape", "我的披风");
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(
            (reg.discovered_packs, reg.accepted, reg.rejected),
            (1, 1, 0),
            "{:?}",
            reg.diagnostics
        );
        assert_eq!(reg.discovered_roots, 1);
        assert_eq!(reg.items.len(), 1);
        assert_eq!(reg.items[0]["title"], "我的披风");
        assert!(
            reg.diagnostics
                .iter()
                .any(|x| x.contains("CAPE_VALIDATED 我的披风1"))
        );
        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn unpacked_folder_and_zip_with_same_name_share_identity() {
        // 相同标准 UUID + 相同稳定键 → 相同 item UUID → 去重。
        let root = std::env::temp_dir().join(format!("cosmetics_duproot_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        pack_zip(&root, "MyCape.zip", "my_cape", "My Cape");
        write_folder_pack(&root, "MyCape", "my_cape", "My Cape");
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(
            (reg.discovered_packs, reg.accepted, reg.rejected),
            (2, 1, 1),
            "{:?}",
            reg.diagnostics
        );
        assert_eq!(reg.items.len(), 1);
        assert!(
            reg.diagnostics
                .iter()
                .any(|x| x.contains("CAPE_REJECTED MyCape"))
        );
        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn folder_without_pack_files_is_skipped_not_rejected() {
        let root = std::env::temp_dir().join(format!("cosmetics_dirskip_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("capes").join("not-a-pack");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("readme.txt"), b"hello").unwrap();
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(
            (reg.discovered_packs, reg.accepted, reg.rejected),
            (0, 0, 0),
            "{:?}",
            reg.diagnostics
        );
        assert!(
            reg.diagnostics
                .iter()
                .any(|x| x.contains("CAPE_FOLDER_SKIPPED not-a-pack"))
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn per_cape_author_and_manifest_authors_are_used_for_folder_offers() {
        let root = std::env::temp_dir().join(format!("cosmetics_authors_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("capes").join("Authors");
        fs::create_dir_all(&dir).unwrap();
        let manifest = json!({
            "format_version": 1,
            "header": {"name":"Authors","uuid":TEST_PACK_UUID,"version":[1,0,0]},
            "modules": [{"type":"cape_pack","uuid":TEST_MODULE_UUID,"version":[1,0,0]}],
            "metadata": {"authors":["Pack Artist", "Coauthor"]}
        });
        let sheet = json!({
            "localization_name":"authors",
            "capes":[
                {"localization_name":"first","texture":"first.png"},
                {"localization_name":"second","texture":"second.png","author":"Cape Artist"}
            ]
        });
        fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        fs::write(dir.join("cape.json"), serde_json::to_vec(&sheet).unwrap()).unwrap();
        fs::write(dir.join("first.png"), dummy_png()).unwrap();
        fs::write(dir.join("second.png"), dummy_png()).unwrap();
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!((reg.accepted, reg.rejected, reg.items.len()), (1, 0, 2), "{:?}", reg.diagnostics);
        for (item, expected) in reg.items.iter().zip(["Pack Artist, Coauthor", "Cape Artist"]) {
            assert_eq!(item["creatorName"], expected);
            let id = item["id"].as_str().unwrap();
            let catalog: Value = serde_json::from_slice(reg.catalogs.get(id).unwrap()).unwrap();
            assert_eq!(catalog["data"]["Item"]["DisplayProperties"]["creatorName"], expected);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn zip_pack_author_and_per_cape_author_have_independent_precedence() {
        let root = std::env::temp_dir().join(format!("cosmetics_zipauthors_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("capes")).unwrap();
        let manifest = json!({
            "format_version":1,
            "header":{"name":"Authors","uuid":TEST_PACK_UUID,"version":[1,0,0]},
            "modules":[{"type":"cape_pack","uuid":TEST_MODULE_UUID,"version":[1,0,0]}],
            "metadata":{"authors":["Manifest Artist"]}
        });
        let sheet = json!({
            "localization_name":"authors",
            "author":"Pack Studio",
            "capes":[
                {"localization_name":"first","texture":"cape.png"},
                {"localization_name":"second","texture":"cape.png","author":"  Individual Artist  "}
            ]
        });
        let mut zip = ZipWriter::new(File::create(root.join("capes/authors.zip")).unwrap());
        let options = SimpleFileOptions::default();
        for (path, data) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("cape.json", serde_json::to_vec(&sheet).unwrap()),
            ("cape.png", dummy_png()),
        ] {
            zip.start_file(path, options).unwrap();
            zip.write_all(&data).unwrap();
        }
        zip.finish().unwrap();
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!((reg.accepted, reg.rejected, reg.items.len()), (1, 0, 2), "{:?}", reg.diagnostics);
        assert_eq!(reg.items[0]["creatorName"], "Pack Studio");
        assert_eq!(reg.items[1]["creatorName"], "Individual Artist");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn localized_pack_author_and_invalid_per_cape_author() {
        let mut files = PackFiles { members: BTreeMap::new() };
        let manifest = json!({
            "format_version":1,
            "header":{"name":"Example","uuid":TEST_PACK_UUID,"version":[1,0,0]},
            "modules":[{"type":"cape_pack","uuid":TEST_MODULE_UUID,"version":[1,0,0]}]
        });
        files.members.insert("manifest.json".into(), serde_json::to_vec(&manifest).unwrap());
        files.members.insert(
            "cape.json".into(),
            br#"{"localization_name":"example","capes":[{"localization_name":"red","texture":"cape.png"}]}"#.to_vec(),
        );
        files.members.insert("texts/en_US.lang".into(), b"capepack.example.by=English Author\n".to_vec());
        files.members.insert("texts/zh_CN.lang".into(), "capepack.example.by=中文作者\n".as_bytes().to_vec());
        let output = standard_pack_manifest(&files, "zh_CN", None).unwrap();
        let pack: PackManifest = serde_json::from_slice(&output).unwrap();
        assert_eq!(pack.author, "中文作者");
        files.members.insert(
            "cape.json".into(),
            br#"{"localization_name":"example","capes":[{"localization_name":"red","texture":"cape.png","author":123}]}"#.to_vec(),
        );
        assert!(standard_pack_manifest(&files, "zh_CN", None).is_err());
    }

    #[test]
    fn multi_cape_folder_pack_registers_all_items() {
        let root = std::env::temp_dir().join(format!("cosmetics_dirmulti_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("capes").join("Collection");
        fs::create_dir_all(dir.join("textures")).unwrap();
        let manifest = json!({
            "format_version":1,
            "header":{"name":"My Folder Collection","uuid":TEST_PACK_UUID,"version":[1,0,0]},
            "modules":[{"type":"cape_pack","uuid":TEST_MODULE_UUID,"version":[1,0,0]}]
        });
        let sheet = json!({
            "serialize_name":"My Folder Collection","localization_name":"my_folder_collection",
            "capes":[
                {"localization_name":"red","texture":"textures/red.png"},
                {"localization_name":"blue","texture":"textures/blue.png"}
            ]
        });
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(dir.join("cape.json"), serde_json::to_vec(&sheet).unwrap()).unwrap();
        fs::create_dir_all(dir.join("texts")).unwrap();
        fs::write(dir.join("texts/en_US.lang"),
            "capepack.my_folder_collection=My Folder Collection\ncape.my_folder_collection.red=Red Cape\ncape.my_folder_collection.blue=Blue Cape\n").unwrap();
        fs::write(dir.join("textures/red.png"), dummy_png()).unwrap();
        fs::write(dir.join("textures/blue.png"), dummy_png()).unwrap();
        let reg = Registry::scan(&[root.clone()], "en_US");
        assert_eq!(
            (reg.discovered_packs, reg.accepted, reg.rejected),
            (1, 1, 0),
            "{:?}",
            reg.diagnostics
        );
        assert_eq!(reg.items.len(), 2);
        assert_eq!(reg.items[0]["title"], "Red Cape");
        assert_eq!(reg.items[1]["title"], "Blue Cape");
        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn multiple_roots_are_scanned_in_order_and_deduplicated() {
        let first = std::env::temp_dir().join(format!("cosmetics_rootA_{}", std::process::id()));
        let second = std::env::temp_dir().join(format!("cosmetics_rootB_{}", std::process::id()));
        let _ = fs::remove_dir_all(&first);
        let _ = fs::remove_dir_all(&second);
        pack_zip(&first, "Shared.zip", "shared", "Shared Cape");
        write_folder_pack(&first, "OnlyA", "only_a", "Only A");
        write_folder_pack(&second, "Shared", "shared", "Shared Cape");
        pack_zip(&second, "OnlyB.zip", "only_b", "Only B");
        let reg = Registry::scan(
            &[first.clone(), second.clone()],
            "en_US",
        );
        assert_eq!(reg.discovered_roots, 2, "{:?}", reg.diagnostics);
        assert_eq!(reg.discovered_packs, 4);
        // Shared 披风来自优先级更高的第一根；第二根的重复包被拒绝。
        assert_eq!(
            (reg.accepted, reg.rejected),
            (3, 1),
            "{:?}",
            reg.diagnostics
        );
        let titles: Vec<String> = reg
            .items
            .iter()
            .filter_map(|item| item["title"].as_str().map(ToOwned::to_owned))
            .collect();
        assert!(titles.contains(&"Shared Cape".to_string()));
        assert!(titles.contains(&"Only B".to_string()));
        assert_eq!(reg.items.len(), 3);
        let _ = fs::remove_dir_all(first);
        let _ = fs::remove_dir_all(second);
    }

}
