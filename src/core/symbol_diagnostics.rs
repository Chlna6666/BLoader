use crate::core::symbols::{self, LoadReport};

pub fn format_snapshot(report: &LoadReport) -> String {
    let game_name = report
        .fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.exe_name.as_str())
        .unwrap_or("<unknown>");
    let game_hash = report
        .fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.sha256.as_str())
        .unwrap_or("<unknown>");
    let pack_id = report.pack_id.as_deref().unwrap_or("<none>");
    let native_hud = crate::core::native_hud_discovery::snapshot();

    format!(
        "status={} | public_symbols={} | pack={} | game={} | game_sha256={} | pack_dir={} | native_hud_status={} | native_hud_candidates={} | native_hud_reason={}",
        report.message,
        report.resolved_symbols,
        pack_id,
        game_name,
        game_hash,
        report.directory.display(),
        native_hud.status,
        native_hud.candidate_count,
        native_hud.reason
    )
}

pub fn is_ready() -> bool {
    let report = symbols::report();
    report.pack_id.is_some() && !report.discovery_only
}

pub fn check_requirements(
    requires_symbol_pack: bool,
    required_symbols: &[String],
) -> Result<(), String> {
    evaluate_requirements(
        requires_symbol_pack,
        required_symbols,
        is_ready(),
        symbols::resolve,
    )
}

pub fn evaluate_requirements(
    requires_symbol_pack: bool,
    required_symbols: &[String],
    pack_loaded: bool,
    resolve_symbol: impl Fn(&str) -> usize,
) -> Result<(), String> {
    if requires_symbol_pack && !pack_loaded {
        return Err("missing matching external symbol pack".to_string());
    }
    let missing = required_symbols
        .iter()
        .filter(|name| resolve_symbol(name) == 0)
        .map(String::as_str)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("missing required symbols: {}", missing.join(", ")))
    }
}
