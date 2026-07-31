use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use flate2::read::DeflateDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const FORMAT_VERSION: u32 = 1;
const MAX_UNCOMPRESSED_PACK_BYTES: usize = 8 * 1024 * 1024;
pub const COMPRESSED_PACK_MAGIC: &[u8; 8] = b"BLSYM01\0";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameFingerprint {
    pub exe_name: String,
    pub sha256: String,
}

impl GameFingerprint {
    pub fn from_path_and_sha256(path: &Path, sha256: impl Into<String>) -> Option<Self> {
        Some(Self {
            exe_name: path.file_name()?.to_string_lossy().to_string(),
            sha256: sha256.into().to_ascii_lowercase(),
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExternalSymbol {
    pub name: String,
    kind: String,
    #[serde(default)]
    rva: Option<String>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    validate: Option<String>,
    #[serde(default)]
    expose_to_mods: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct PackTarget {
    exe_name: String,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct NativeHudPack {
    #[serde(default)]
    candidates: Vec<NativeHudCandidate>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NativeHudCandidate {
    pub(crate) name: String,
    pub(crate) pattern: String,
    pub(crate) section: String,
    pub(crate) validate: String,
    pub(crate) abi: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExternalSymbolPack {
    format_version: u32,
    id: String,
    target: PackTarget,
    #[serde(default)]
    discovery_only: bool,
    symbols: Vec<ExternalSymbol>,
    #[serde(default)]
    native_hud: NativeHudPack,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SymbolPackError {
    InvalidJson(String),
    InvalidCompressedHeader,
    InvalidCompressedLength,
    DecompressionFailed(String),
    UnsupportedFormatVersion(u32),
    MissingField(&'static str),
    InvalidRva { name: String, value: String },
    UnsupportedSymbolKind { name: String, kind: String },
    MissingResolution { name: String, kind: String },
    DuplicateSymbol(String),
    DiscoveryPackHasSymbols,
    InvalidNativeHudCandidate { name: String, reason: &'static str },
    DuplicateNativeHudCandidate(String),
}

impl fmt::Display for SymbolPackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(error) => write!(f, "invalid JSON: {error}"),
            Self::InvalidCompressedHeader => write!(f, "invalid compressed symbol-pack header"),
            Self::InvalidCompressedLength => write!(f, "invalid compressed symbol-pack length"),
            Self::DecompressionFailed(error) => {
                write!(f, "symbol-pack decompression failed: {error}")
            }
            Self::UnsupportedFormatVersion(version) => {
                write!(f, "unsupported format version {version}")
            }
            Self::MissingField(field) => write!(f, "missing required field '{field}'"),
            Self::InvalidRva { name, value } => {
                write!(f, "symbol '{name}' has invalid RVA '{value}'")
            }
            Self::UnsupportedSymbolKind { name, kind } => {
                write!(f, "symbol '{name}' has unsupported kind '{kind}'")
            }
            Self::MissingResolution { name, kind } => {
                write!(f, "symbol '{name}' of kind '{kind}' has no resolution data")
            }
            Self::DuplicateSymbol(name) => write!(f, "symbol '{name}' is declared more than once"),
            Self::DiscoveryPackHasSymbols => {
                write!(f, "a discovery-only pack cannot declare ordinary symbols")
            }
            Self::InvalidNativeHudCandidate { name, reason } => {
                write!(f, "native HUD candidate '{name}' is invalid: {reason}")
            }
            Self::DuplicateNativeHudCandidate(name) => {
                write!(
                    f,
                    "native HUD candidate '{name}' is declared more than once"
                )
            }
        }
    }
}

impl std::error::Error for SymbolPackError {}

impl ExternalSymbolPack {
    pub fn from_json(json: &str) -> Result<Self, SymbolPackError> {
        let pack: Self = serde_json::from_str(json)
            .map_err(|error| SymbolPackError::InvalidJson(error.to_string()))?;
        pack.validate()?;
        Ok(pack)
    }

    pub fn from_compressed_bytes(bytes: &[u8]) -> Result<Self, SymbolPackError> {
        let header_length = COMPRESSED_PACK_MAGIC.len() + std::mem::size_of::<u32>();
        if bytes.len() <= header_length || !bytes.starts_with(COMPRESSED_PACK_MAGIC) {
            return Err(SymbolPackError::InvalidCompressedHeader);
        }
        let declared_length = u32::from_le_bytes(
            bytes[COMPRESSED_PACK_MAGIC.len()..header_length]
                .try_into()
                .map_err(|_| SymbolPackError::InvalidCompressedHeader)?,
        ) as usize;
        if declared_length == 0 || declared_length > MAX_UNCOMPRESSED_PACK_BYTES {
            return Err(SymbolPackError::InvalidCompressedLength);
        }

        let mut json = Vec::with_capacity(declared_length);
        DeflateDecoder::new(&bytes[header_length..])
            .read_to_end(&mut json)
            .map_err(|error| SymbolPackError::DecompressionFailed(error.to_string()))?;
        if json.len() != declared_length {
            return Err(SymbolPackError::InvalidCompressedLength);
        }
        let json = String::from_utf8(json)
            .map_err(|error| SymbolPackError::InvalidJson(error.to_string()))?;
        Self::from_json(&json)
    }

    pub fn matches(&self, fingerprint: &GameFingerprint) -> bool {
        if !self
            .target
            .exe_name
            .eq_ignore_ascii_case(&fingerprint.exe_name)
        {
            return false;
        }
        self.target
            .sha256
            .as_deref()
            .map(|sha256| sha256.eq_ignore_ascii_case(&fingerprint.sha256))
            .unwrap_or(self.discovery_only)
    }

    pub fn symbols(&self) -> &[ExternalSymbol] {
        &self.symbols
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn native_hud_candidates(&self) -> &[NativeHudCandidate] {
        &self.native_hud.candidates
    }

    pub fn is_discovery_only(&self) -> bool {
        self.discovery_only
    }

    fn is_generic_discovery_pack(&self) -> bool {
        self.discovery_only && self.target.sha256.is_none()
    }

    fn validate(&self) -> Result<(), SymbolPackError> {
        if self.format_version != FORMAT_VERSION {
            return Err(SymbolPackError::UnsupportedFormatVersion(
                self.format_version,
            ));
        }
        if self.id.trim().is_empty() {
            return Err(SymbolPackError::MissingField("id"));
        }
        if self.target.exe_name.trim().is_empty() {
            return Err(SymbolPackError::MissingField("target.exe_name"));
        }
        if !self.discovery_only && self.target.sha256.as_deref().is_none_or(str::is_empty) {
            return Err(SymbolPackError::MissingField("target.sha256"));
        }
        if self.discovery_only && !self.symbols.is_empty() {
            return Err(SymbolPackError::DiscoveryPackHasSymbols);
        }

        let mut names = BTreeSet::new();
        for symbol in &self.symbols {
            if symbol.name.trim().is_empty() {
                return Err(SymbolPackError::MissingField("symbols[].name"));
            }
            if !names.insert(&symbol.name) {
                return Err(SymbolPackError::DuplicateSymbol(symbol.name.clone()));
            }
            match symbol.kind.as_str() {
                "rva" => {
                    let value = symbol.rva.as_deref().ok_or_else(|| {
                        SymbolPackError::MissingResolution {
                            name: symbol.name.clone(),
                            kind: symbol.kind.clone(),
                        }
                    })?;
                    parse_rva(value).map_err(|_| SymbolPackError::InvalidRva {
                        name: symbol.name.clone(),
                        value: value.to_string(),
                    })?;
                }
                "pattern" => {
                    if symbol.pattern.as_deref().is_none_or(str::is_empty) {
                        return Err(SymbolPackError::MissingResolution {
                            name: symbol.name.clone(),
                            kind: symbol.kind.clone(),
                        });
                    }
                }
                _ => {
                    return Err(SymbolPackError::UnsupportedSymbolKind {
                        name: symbol.name.clone(),
                        kind: symbol.kind.clone(),
                    });
                }
            }
        }

        let mut candidate_names = BTreeSet::new();
        for candidate in &self.native_hud.candidates {
            if candidate.name.trim().is_empty() {
                return Err(SymbolPackError::InvalidNativeHudCandidate {
                    name: String::new(),
                    reason: "name is empty",
                });
            }
            if !candidate_names.insert(&candidate.name) {
                return Err(SymbolPackError::DuplicateNativeHudCandidate(
                    candidate.name.clone(),
                ));
            }
            let invalid = |reason| SymbolPackError::InvalidNativeHudCandidate {
                name: candidate.name.clone(),
                reason,
            };
            if candidate.pattern.trim().is_empty() {
                return Err(invalid("pattern is empty"));
            }
            if candidate.section != ".text" {
                return Err(invalid("section must be .text"));
            }
            if candidate.validate.trim().is_empty() {
                return Err(invalid("validation bytes are empty"));
            }
            if candidate.abi.trim().is_empty() {
                return Err(invalid("ABI declaration is empty"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct LoadReport {
    pub directory: PathBuf,
    pub fingerprint: Option<GameFingerprint>,
    pub pack_id: Option<String>,
    pub resolved_symbols: usize,
    pub discovery_only: bool,
    pub message: String,
}

impl Default for LoadReport {
    fn default() -> Self {
        Self {
            directory: PathBuf::new(),
            fingerprint: None,
            pack_id: None,
            resolved_symbols: 0,
            discovery_only: false,
            message: "not initialized".to_string(),
        }
    }
}

#[derive(Default)]
struct SymbolResolver {
    values: BTreeMap<String, usize>,
    pack: Option<ExternalSymbolPack>,
    report: LoadReport,
}

static RESOLVER: OnceLock<Mutex<SymbolResolver>> = OnceLock::new();

fn resolver() -> &'static Mutex<SymbolResolver> {
    RESOLVER.get_or_init(|| Mutex::new(SymbolResolver::default()))
}

pub fn initialize_current_process(loader_directory: &Path) -> LoadReport {
    let report = match std::env::current_exe() {
        Ok(executable) => initialize_for_executable(&executable, loader_directory),
        Err(error) => LoadReport {
            message: format!("could not locate game executable: {error}"),
            ..LoadReport::default()
        },
    };
    let mut state = resolver().lock().unwrap_or_else(|error| error.into_inner());
    if report.pack_id.is_none() {
        state.values.clear();
        state.pack = None;
    }
    state.report = report.clone();
    report
}

pub fn resolve(name: &str) -> usize {
    let state = resolver().lock().unwrap_or_else(|error| error.into_inner());
    state.values.get(name).copied().unwrap_or(0)
}

pub fn report() -> LoadReport {
    resolver()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .report
        .clone()
}

pub fn public_symbol_names() -> Vec<String> {
    resolver()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .values
        .keys()
        .cloned()
        .collect()
}

pub fn loaded_pack() -> Option<ExternalSymbolPack> {
    resolver()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .pack
        .clone()
}

pub(crate) fn validation_matches_loaded_image(address: usize, pattern: &str) -> bool {
    let Some((base, image_size)) = crate::core::sig_scan::image_bounds() else {
        return false;
    };
    matches_bytes(address, pattern, base, image_size)
}

fn initialize_for_executable(executable: &Path, loader_directory: &Path) -> LoadReport {
    let directory = loader_directory.to_path_buf();
    let fingerprint = match fingerprint_file(executable) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            return LoadReport {
                directory,
                message: format!("could not fingerprint {}: {error}", executable.display()),
                ..LoadReport::default()
            };
        }
    };

    let pack = match matching_pack(&directory, &fingerprint) {
        Ok(Some(pack)) => pack,
        Ok(None) => {
            return LoadReport {
                directory,
                fingerprint: Some(fingerprint),
                message: "no external symbol pack matches this game build".to_string(),
                ..LoadReport::default()
            };
        }
        Err(error) => {
            return LoadReport {
                directory,
                fingerprint: Some(fingerprint),
                message: error,
                ..LoadReport::default()
            };
        }
    };

    let Some((base, image_size)) = crate::core::sig_scan::image_bounds() else {
        return LoadReport {
            directory,
            fingerprint: Some(fingerprint),
            message: "could not read the loaded game image bounds".to_string(),
            ..LoadReport::default()
        };
    };

    let mut values = BTreeMap::new();
    let mut failures = Vec::new();
    for symbol in &pack.symbols {
        let address = match resolve_symbol(symbol, base, image_size) {
            Ok(address) => address,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };
        if symbol.expose_to_mods {
            values.insert(symbol.name.clone(), address);
        }
    }

    let message = if failures.is_empty() {
        format!(
            "loaded external symbol pack '{}': {} public symbols",
            pack.id,
            values.len()
        )
    } else {
        format!(
            "loaded external symbol pack '{}': {} public symbols; {} entries rejected",
            pack.id,
            values.len(),
            failures.len()
        )
    };
    let mut state = resolver().lock().unwrap_or_else(|error| error.into_inner());
    state.values = values.clone();
    state.pack = Some(pack.clone());
    LoadReport {
        directory,
        fingerprint: Some(fingerprint),
        pack_id: Some(pack.id.clone()),
        resolved_symbols: values.len(),
        discovery_only: pack.is_discovery_only(),
        message,
    }
}

fn fingerprint_file(path: &Path) -> io::Result<GameFingerprint> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    GameFingerprint::from_path_and_sha256(path, format!("{:x}", hasher.finalize())).ok_or_else(
        || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "executable path has no file name",
            )
        },
    )
}

fn matching_pack(
    directory: &Path,
    fingerprint: &GameFingerprint,
) -> Result<Option<ExternalSymbolPack>, String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read {}: {error}", directory.display())),
    };

    let mut exact_candidates = Vec::new();
    let mut generic_candidates = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("blsym"))
        {
            continue;
        }
        let bytes = fs::read(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let pack = ExternalSymbolPack::from_compressed_bytes(&bytes)
            .map_err(|error| format!("invalid symbol pack {}: {error}", path.display()))?;
        if pack.matches(fingerprint) {
            if pack.is_generic_discovery_pack() {
                generic_candidates.push(pack);
            } else {
                exact_candidates.push(pack);
            }
        }
    }
    if exact_candidates.len() > 1 {
        return Err("multiple external symbol packs match this game build".to_string());
    }
    if let Some(pack) = exact_candidates.pop() {
        return Ok(Some(pack));
    }
    if generic_candidates.len() > 1 {
        return Err("multiple generic discovery symbol packs match this game build".to_string());
    }
    Ok(generic_candidates.pop())
}

fn resolve_symbol(
    symbol: &ExternalSymbol,
    base: usize,
    image_size: usize,
) -> Result<usize, String> {
    let address = match symbol.kind.as_str() {
        "rva" => {
            let rva = parse_rva(symbol.rva.as_deref().unwrap_or_default())
                .map_err(|_| format!("{} has an invalid RVA", symbol.name))?;
            if rva >= image_size {
                return Err(format!("{} RVA is outside the game image", symbol.name));
            }
            base + rva
        }
        "pattern" => {
            crate::core::sig_scan::scan_unique(symbol.pattern.as_deref().unwrap_or_default())
                .ok_or_else(|| format!("{} pattern did not match uniquely", symbol.name))?
        }
        _ => return Err(format!("{} has an unsupported kind", symbol.name)),
    };
    if let Some(validation) = symbol.validate.as_deref() {
        if !matches_bytes(address, validation, base, image_size) {
            return Err(format!("{} failed validation bytes", symbol.name));
        }
    }
    Ok(address)
}

fn matches_bytes(address: usize, pattern: &str, base: usize, image_size: usize) -> bool {
    let Some(bytes) = parse_exact_pattern(pattern) else {
        return false;
    };
    let image_end = base.saturating_add(image_size);
    if address < base || address.saturating_add(bytes.len()) > image_end {
        return false;
    }
    unsafe {
        bytes
            .iter()
            .enumerate()
            .all(|(offset, expected)| *(address as *const u8).add(offset) == *expected)
    }
}

fn parse_rva(value: &str) -> Result<usize, ()> {
    let value = value.trim();
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    usize::from_str_radix(value, 16).map_err(|_| ())
}

fn parse_exact_pattern(pattern: &str) -> Option<Vec<u8>> {
    pattern
        .split_whitespace()
        .map(|part| u8::from_str_radix(part, 16).ok())
        .collect()
}
