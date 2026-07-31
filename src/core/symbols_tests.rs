use std::path::Path;

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;

use super::symbol_diagnostics::{evaluate_requirements, format_snapshot};
use super::symbols::{
    COMPRESSED_PACK_MAGIC, ExternalSymbolPack, GameFingerprint, LoadReport, SymbolPackError,
};

const PACK: &str = r#"{
  "format_version": 1,
  "id": "example.minecraft.windows.26.21",
  "target": {
    "exe_name": "Minecraft.Windows.exe",
    "sha256": "48e1f498eac0ce00a9de1f91773ee4510b507c39a55fd09f9c5dd07da0982f3f"
  },
  "symbols": [
    {
      "name": "runtime.game_module_base",
      "kind": "rva",
      "rva": "0x0",
      "validate": "4D 5A"
    }
  ]
}"#;

const NATIVE_HUD_PACK: &str = r#"{
  "format_version": 1,
  "id": "example.minecraft.windows.26.21.native-hud-discovery",
  "target": {
    "exe_name": "Minecraft.Windows.exe",
    "sha256": "48e1f498eac0ce00a9de1f91773ee4510b507c39a55fd09f9c5dd07da0982f3f"
  },
  "symbols": [],
  "native_hud": {
    "candidates": [{
      "name": "hud.version.draw",
      "pattern": "48 89 5C 24 ?? 57 48 83 EC ??",
      "section": ".text",
      "validate": "48 89 5C 24",
      "abi": "x64-msvc: trace-only"
    }]
  }
}"#;

const INVALID_NATIVE_HUD_SECTION: &str = r#"{
  "format_version": 1,
  "id": "example.invalid-native-hud",
  "target": {
    "exe_name": "Minecraft.Windows.exe",
    "sha256": "48e1f498eac0ce00a9de1f91773ee4510b507c39a55fd09f9c5dd07da0982f3f"
  },
  "symbols": [],
  "native_hud": {
    "candidates": [{
      "name": "hud.version.draw",
      "pattern": "48 89 5C 24 ?? 57 48 83 EC ??",
      "section": ".rdata",
      "validate": "48 89 5C 24",
      "abi": "x64-msvc: trace-only"
    }]
  }
}"#;

const GENERIC_DISCOVERY_PACK: &str = r#"{
  "format_version": 1,
  "id": "example.minecraft.windows.native-hud-discovery",
  "discovery_only": true,
  "target": {
    "exe_name": "Minecraft.Windows.exe"
  },
  "symbols": [],
  "native_hud": { "candidates": [] }
}"#;

#[test]
fn external_pack_matches_the_target_fingerprint() {
    let pack = ExternalSymbolPack::from_json(PACK).expect("valid external symbol pack");
    let fingerprint = GameFingerprint {
        exe_name: "Minecraft.Windows.exe".to_string(),
        sha256: "48e1f498eac0ce00a9de1f91773ee4510b507c39a55fd09f9c5dd07da0982f3f".to_string(),
    };

    assert!(pack.matches(&fingerprint));
    assert_eq!(pack.symbols()[0].name, "runtime.game_module_base");
}

#[test]
fn external_pack_rejects_a_different_executable() {
    let pack = ExternalSymbolPack::from_json(PACK).expect("valid external symbol pack");
    let fingerprint = GameFingerprint {
        exe_name: "Minecraft.Windows.exe".to_string(),
        sha256: "different-build".to_string(),
    };

    assert!(!pack.matches(&fingerprint));
}

#[test]
fn external_pack_rejects_unknown_symbol_kinds() {
    let json = PACK.replace("\"rva\"", "\"unknown\"");
    let error = ExternalSymbolPack::from_json(&json).expect_err("invalid kind must fail");

    assert!(matches!(
        error,
        SymbolPackError::UnsupportedSymbolKind { .. }
    ));
}

#[test]
fn native_hud_candidate_accepts_only_private_text_metadata() {
    let pack = ExternalSymbolPack::from_json(NATIVE_HUD_PACK)
        .expect("native HUD discovery metadata must parse");

    assert_eq!(pack.native_hud_candidates().len(), 1);
    assert!(!pack.is_discovery_only());
    assert!(pack.symbols().is_empty());
}

#[test]
fn native_hud_candidate_rejects_non_text_section() {
    assert!(ExternalSymbolPack::from_json(INVALID_NATIVE_HUD_SECTION).is_err());
}

#[test]
fn generic_discovery_pack_matches_a_new_game_hash_without_symbols() {
    let pack = ExternalSymbolPack::from_json(GENERIC_DISCOVERY_PACK)
        .expect("generic discovery metadata must parse");
    let fingerprint = GameFingerprint {
        exe_name: "Minecraft.Windows.exe".to_string(),
        sha256: "future-game-build".to_string(),
    };

    assert!(pack.is_discovery_only());
    assert!(pack.matches(&fingerprint));
    assert!(pack.symbols().is_empty());
}

#[test]
fn generic_discovery_template_is_private_and_hash_agnostic() {
    let pack = ExternalSymbolPack::from_json(include_str!(
        "../../templates/symbol-packs/minecraft.windows.hud-discovery.example.json"
    ))
    .expect("generic discovery template must parse");

    assert!(pack.is_discovery_only());
    assert!(pack.symbols().is_empty());
    assert!(pack.native_hud_candidates().is_empty());
}

#[test]
fn fingerprint_uses_the_game_executable_name() {
    let fingerprint = GameFingerprint::from_path_and_sha256(
        Path::new(r"C:\\games\\Minecraft.Windows.exe"),
        "hash",
    )
    .expect("file name exists");

    assert_eq!(fingerprint.exe_name, "Minecraft.Windows.exe");
}

#[test]
fn compressed_pack_decodes_the_embedded_json() {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(PACK.as_bytes())
        .expect("compress source pack");
    let compressed = encoder.finish().expect("finish compression");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(COMPRESSED_PACK_MAGIC);
    bytes.extend_from_slice(&(PACK.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&compressed);

    let pack = ExternalSymbolPack::from_compressed_bytes(&bytes).expect("decode pack");

    assert_eq!(pack.id(), "example.minecraft.windows.26.21");
}

#[test]
fn requirement_gate_reports_missing_pack_and_symbols() {
    let no_symbols = Vec::new();
    let client_instance = vec!["client.instance".to_string()];
    let missing_pack = evaluate_requirements(true, &no_symbols, false, |_| 0)
        .expect_err("symbol-pack requirement must fail without a pack");
    assert_eq!(missing_pack, "missing matching external symbol pack");

    let missing_symbol = evaluate_requirements(false, &client_instance, true, |_| 0)
        .expect_err("missing symbols must block the mod");
    assert_eq!(missing_symbol, "missing required symbols: client.instance");

    assert!(evaluate_requirements(false, &client_instance, true, |_| 0x1400_0000_0).is_ok());
}

#[test]
fn diagnostic_snapshot_includes_symbol_pack_state() {
    let snapshot = format_snapshot(&LoadReport::default());

    assert!(snapshot.contains("status=not initialized"));
    assert!(snapshot.contains("public_symbols=0"));
}
