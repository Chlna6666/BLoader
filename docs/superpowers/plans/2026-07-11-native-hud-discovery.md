# Native HUD Discovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a safe, external-symbol-pack-driven discovery mode for the Minecraft Windows 26.21 native HUD text path without installing a game hook.

**Architecture:** The BLSYM parser gains structured, non-public native-HUD discovery declarations and supports a generic discovery-only package selected by executable name. A section-aware scanner resolves only unique `.text` candidates and the discovery module records validation evidence without calling game code. The host exposes the resulting status through `ui.native_hud.*`; no draw detour, memory patch, or public MOD address is added.

**Tech Stack:** Rust 2024, Windows PE headers, existing `minhook`-independent symbol resolver, `serde`, BLSYM Deflate package format, cargo tests.

---

## File Structure

- Create: `src/core/native_hud_discovery.rs` - evaluates declared HUD candidates and keeps a diagnostic snapshot.
- Modify: `src/core/sig_scan.rs` - parse PE section bounds and scan only named image sections.
- Modify: `src/core/symbols.rs` - deserialize and validate `native_hud` pack metadata and generic discovery-only target rules without exposing addresses to MODs.
- Modify: `src/core/symbol_diagnostics.rs` - include native HUD state in the loader diagnostic snapshot.
- Modify: `src/core/mod.rs` - register the discovery module.
- Modify: `src/lib.rs` - run discovery after a matching BLSYM package has initialized.
- Modify: `src/bl/host.rs` - publish read-only `ui.native_hud.status`, `mode`, `candidate_count`, and `reason` values.
- Modify: `src/core/symbols_tests.rs` and `src/bl/host.rs` tests - cover package metadata, section restrictions, rejected candidates, and runtime-info exposure.
- Modify: `templates/symbol-packs/minecraft.windows.26.21.example.json` and `templates/symbol-packs/README.md` - document empty discovery metadata and the evidence required before a rendering declaration is allowed.

### Task 1: Scan an explicit PE section

**Files:**
- Modify: `src/core/sig_scan.rs`
- Test: `src/core/symbols_tests.rs`

- [ ] **Step 1: Write a failing scanner section-name parser test**

Add a private parser test for an `IMAGE_SECTION_HEADER` name with a NUL suffix:

```rust
#[test]
fn section_name_stops_at_the_first_nul() {
    assert_eq!(section_name([b'.', b't', b'e', b'x', b't', 0, 0, 0]), ".text");
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test section_name_stops_at_the_first_nul`

Expected: failure because `section_name` does not exist.

- [ ] **Step 3: Implement section-bound lookup and section scanning**

Add these APIs in `src/core/sig_scan.rs` without changing `scan_unique`:

```rust
pub fn section_bounds(section_name: &str) -> Option<(usize, usize)>;
pub fn scan_unique_in_section(signature: &str, section_name: &str) -> Option<usize>;

fn section_name(raw: [u8; 8]) -> String {
    let length = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..length]).to_string()
}
```

Use `IMAGE_FIRST_SECTION`-equivalent pointer arithmetic from the NT headers, require the requested section range to be wholly inside `image_bounds`, and reuse a range-based scanner that accepts `(base, size)`. Return `None` for malformed PE metadata, a missing section, an empty pattern, or anything other than exactly one match.

- [ ] **Step 4: Run the focused test**

Run: `cargo test section_name_stops_at_the_first_nul`

Expected: PASS.

- [ ] **Step 5: Commit the focused scanner change**

```powershell
git add src/core/sig_scan.rs src/core/symbols_tests.rs
git commit -m "feat: scan unique signatures inside PE sections"
```

### Task 2: Add non-public BLSYM HUD discovery declarations

**Files:**
- Modify: `src/core/symbols.rs`
- Modify: `src/core/symbols_tests.rs`

- [ ] **Step 1: Write failing BLSYM metadata tests**

Add JSON fixtures asserting that a valid discovery candidate must name a `.text` section, use a non-empty pattern and validation byte sequence, and is not resolved through `resolve`:

```rust
assert!(ExternalSymbolPack::from_json(VALID_NATIVE_HUD_PACK).is_ok());
assert!(ExternalSymbolPack::from_json(INVALID_NATIVE_HUD_SECTION).is_err());
assert_eq!(pack.native_hud_candidates().len(), 1);
```

Use `"section":".rdata"` in `INVALID_NATIVE_HUD_SECTION` and name the test `native_hud_candidate_rejects_non_text_section`.

- [ ] **Step 2: Run the focused tests and verify failure**

Run: `cargo test native_hud`

Expected: failure because BLSYM has no `native_hud` schema.

- [ ] **Step 3: Implement the schema and validation**

Add optional fields to `ExternalSymbolPack`:

```rust
#[serde(default)]
native_hud: NativeHudPack,

#[derive(Clone, Debug, Default, Deserialize)]
pub struct NativeHudPack {
    #[serde(default)]
    candidates: Vec<NativeHudCandidate>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NativeHudCandidate {
    name: String,
    pattern: String,
    section: String,
    validate: String,
    abi: String,
}
```

Add `discovery_only: bool` at package level and make `target.sha256` optional only when `discovery_only` is `true`. Reject a generic package that declares ordinary `symbols`, reject an exact package with a missing SHA-256, reject a blank name/pattern/validate/abi, a duplicate candidate name, and any section other than `.text`. When both exact and generic packs match a game executable, select the exact pack; reject multiple packs at the same precedence. Add `ExternalSymbolPack::native_hud_candidates(&self) -> &[NativeHudCandidate]` and `ExternalSymbolPack::is_discovery_only() -> bool`. Do not add candidates to `SymbolResolver.values`, and do not permit `expose_to_mods` in this schema.

- [ ] **Step 4: Run metadata tests**

Run: `cargo test native_hud`

Expected: PASS; existing symbol-pack tests remain green.

- [ ] **Step 5: Commit the BLSYM schema change**

```powershell
git add src/core/symbols.rs src/core/symbols_tests.rs
git commit -m "feat: describe private native HUD discovery candidates"
```

### Task 3: Implement discovery-only evidence collection

**Files:**
- Create: `src/core/native_hud_discovery.rs`
- Modify: `src/core/mod.rs`
- Modify: `src/lib.rs`
- Test: `src/core/native_hud_discovery.rs`

- [ ] **Step 1: Write failing state-selection tests**

Create tests for the pure decision function:

```rust
#[test]
fn no_pack_candidates_reports_unavailable() {
    assert_eq!(select_status(&[]), NativeHudStatus::Unavailable);
}

#[test]
fn a_uniquely_validated_candidate_reports_trace_ready() {
    let evidence = CandidateEvidence::validated("hud.version.draw", 0x140001000);
    assert_eq!(select_status(&[evidence]), NativeHudStatus::TraceReady);
}
```

- [ ] **Step 2: Run the test and verify failure**

Run: `cargo test native_hud_discovery`

Expected: failure because the module does not exist.

- [ ] **Step 3: Implement a no-hook discovery module**

Define private evidence and public read-only snapshot types:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeHudStatus { Unavailable, Rejected, TraceReady }

#[derive(Clone, Debug, Default)]
pub struct NativeHudSnapshot {
    pub status: NativeHudStatus,
    pub candidate_count: usize,
    pub reason: String,
}

pub fn initialize(pack: Option<&ExternalSymbolPack>) -> NativeHudSnapshot;
pub fn snapshot() -> NativeHudSnapshot;
```

For every candidate, call `scan_unique_in_section(&candidate.pattern, &candidate.section)`, then compare exact bytes through a shared safe resolver helper. Record candidate names, candidate addresses only in loader logs, the ABI declaration, and rejection reasons. `TraceReady` means one or more candidates were uniquely resolved and validated; it does not mean that a hook is installed. Never call the candidate address and never import `minhook` in this module.

Call `native_hud_discovery::initialize(symbols::loaded_pack())` after `initialize_current_process` in `src/lib.rs`; add a `loaded_pack` accessor that returns a clone or immutable snapshot of validated private metadata rather than symbol addresses. The discovery snapshot must include whether the selected pack is exact or generic, and generic mode can never return a status that authorizes rendering.

- [ ] **Step 4: Run discovery tests**

Run: `cargo test native_hud_discovery`

Expected: PASS.

- [ ] **Step 5: Commit discovery-only runtime code**

```powershell
git add src/core/native_hud_discovery.rs src/core/mod.rs src/lib.rs src/core/symbols.rs
git commit -m "feat: add native HUD discovery diagnostics"
```

### Task 4: Publish diagnostics without exposing game addresses

**Files:**
- Modify: `src/core/symbol_diagnostics.rs`
- Modify: `src/bl/host.rs`
- Test: `src/bl/host.rs`

- [ ] **Step 1: Write failing runtime-info tests**

Add a test that public runtime keys include only status values:

```rust
assert!(is_public_runtime_info_key("ui.native_hud.status"));
assert!(is_public_runtime_info_key("ui.native_hud.reason"));
assert!(!is_public_runtime_info_key("ui.native_hud.candidate_address"));
```

- [ ] **Step 2: Run the test and verify failure**

Run: `cargo test exposes_native_hud_runtime_info_keys`

Expected: failure because the host does not recognize those keys.

- [ ] **Step 3: Implement status mapping**

Expose these exact keys in `host_get_runtime_info`:

```rust
"ui.native_hud.status" => native_hud_discovery::snapshot().status.to_string(),
"ui.native_hud.mode" => "discovery".to_string(),
"ui.native_hud.candidate_count" => native_hud_discovery::snapshot().candidate_count.to_string(),
"ui.native_hud.reason" => native_hud_discovery::snapshot().reason,
```

Add the snapshot fields to `symbol_diagnostics::format_snapshot` so crash reports inherit the diagnostic data. Do not expose any resolved address, pattern, ABI string, or candidate identifier through the MOD API.

- [ ] **Step 4: Run host and diagnostics tests**

Run: `cargo test exposes_native_hud_runtime_info_keys`

Expected: PASS.

- [ ] **Step 5: Commit the diagnostics change**

```powershell
git add src/core/symbol_diagnostics.rs src/bl/host.rs
git commit -m "feat: report native HUD discovery status"
```

### Task 5: Provide an intentionally empty 26.21 declaration and deploy discovery mode

**Files:**
- Modify: `templates/symbol-packs/minecraft.windows.26.21.example.json`
- Modify: `templates/symbol-packs/README.md`
- Modify: `tools/pack_symbol_pack.py` only if schema parsing needs explicit source validation

- [ ] **Step 1: Document empty discovery metadata**

Add this exact metadata to the 26.21 example without adding a candidate:

```json
"native_hud": {
  "candidates": []
}
```

Document that real candidates require a unique `.text` pattern, exact validation bytes, an ABI declaration, a trace log proving copyright/version draws, and manual review before an enabled rendering declaration exists.

Create a separate `templates/symbol-packs/minecraft.windows.hud-discovery.example.json` with `"discovery_only": true`, `target.exe_name = "Minecraft.Windows.exe"`, no SHA-256, no ordinary `symbols`, and an initially empty `native_hud.candidates` array. Document that it is safe on future versions because it only produces diagnostics and cannot export symbols or install hooks.

- [ ] **Step 2: Build, package, and verify no native hook is installed**

Run:

```powershell
cargo test --workspace
cargo build
python tools/pack_symbol_pack.py templates/symbol-packs/minecraft.windows.26.21.example.json target/debug/minecraft.windows.26.21.blsym
```

Expected: all tests pass; startup diagnostics report `ui.native_hud.status=unavailable` and `ui.native_hud.mode=discovery` for the empty 26.21 pack.

- [ ] **Step 3: Deploy only after build verification**

```powershell
Copy-Item target/debug/BLoader.dll C:\Users\Administrator\Desktop\BMCBL\target\debug\BMCBL\versions\26.21\BLoader.dll -Force
Copy-Item target/debug/minecraft.windows.26.21.blsym C:\Users\Administrator\Desktop\BMCBL\target\debug\BMCBL\versions\26.21\minecraft.windows.26.21.blsym -Force
```

- [ ] **Step 4: Commit documentation and template updates**

```powershell
git add templates/symbol-packs tools/pack_symbol_pack.py docs/superpowers/plans/2026-07-11-native-hud-discovery.md
git commit -m "docs: document native HUD discovery packages"
```
