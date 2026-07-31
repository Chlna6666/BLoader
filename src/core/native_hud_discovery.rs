use std::fmt;
use std::sync::{Mutex, OnceLock};

use crate::core::symbols::{self, ExternalSymbolPack};
use crate::runtime::foundation::logging;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NativeHudStatus {
    #[default]
    Unavailable,
    Rejected,
    TraceReady,
}

impl fmt::Display for NativeHudStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Unavailable => "unavailable",
            Self::Rejected => "rejected",
            Self::TraceReady => "trace_ready",
        };
        formatter.write_str(value)
    }
}

#[derive(Clone, Debug, Default)]
pub struct NativeHudSnapshot {
    pub status: NativeHudStatus,
    pub candidate_count: usize,
    pub pack_loaded: bool,
    pub discovery_only: bool,
    pub reason: String,
}

#[derive(Clone, Debug)]
struct CandidateEvidence {
    valid: bool,
}

impl CandidateEvidence {
    #[cfg(test)]
    fn validated(_name: &str, _address: usize) -> Self {
        Self { valid: true }
    }
}

static SNAPSHOT: OnceLock<Mutex<NativeHudSnapshot>> = OnceLock::new();

fn state() -> &'static Mutex<NativeHudSnapshot> {
    SNAPSHOT.get_or_init(|| Mutex::new(NativeHudSnapshot::default()))
}

pub fn initialize(pack: Option<&ExternalSymbolPack>) -> NativeHudSnapshot {
    let snapshot = inspect_pack(pack);
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    *state = snapshot.clone();
    snapshot
}

pub fn snapshot() -> NativeHudSnapshot {
    state()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn inspect_pack(pack: Option<&ExternalSymbolPack>) -> NativeHudSnapshot {
    let Some(pack) = pack else {
        return NativeHudSnapshot {
            reason: "no matching external symbol pack".to_string(),
            ..NativeHudSnapshot::default()
        };
    };
    let candidates = pack.native_hud_candidates();
    if candidates.is_empty() {
        return NativeHudSnapshot {
            pack_loaded: true,
            discovery_only: pack.is_discovery_only(),
            reason: "symbol pack declares no native HUD candidates".to_string(),
            ..NativeHudSnapshot::default()
        };
    }

    let mut evidence = Vec::with_capacity(candidates.len());
    let mut rejected = Vec::new();
    for candidate in candidates {
        let Some(address) =
            crate::core::sig_scan::scan_unique_in_section(&candidate.pattern, &candidate.section)
        else {
            rejected.push(format!(
                "{} did not match uniquely in .text",
                candidate.name
            ));
            continue;
        };
        if !symbols::validation_matches_loaded_image(address, &candidate.validate) {
            rejected.push(format!("{} failed validation bytes", candidate.name));
            continue;
        }
        logging::info_message(&format!(
            "[native-hud] trace candidate accepted name={} address=0x{:X} abi={}",
            candidate.name, address, candidate.abi
        ));
        evidence.push(CandidateEvidence { valid: true });
    }

    let status = select_status(&evidence);
    let reason = match status {
        NativeHudStatus::TraceReady => format!(
            "{} candidate(s) passed unique .text scanning and validation; native hook remains disabled",
            evidence.len()
        ),
        NativeHudStatus::Rejected => {
            format!("native HUD candidates rejected: {}", rejected.join("; "))
        }
        NativeHudStatus::Unavailable => "native HUD discovery is unavailable".to_string(),
    };
    NativeHudSnapshot {
        status,
        candidate_count: candidates.len(),
        pack_loaded: true,
        discovery_only: pack.is_discovery_only(),
        reason,
    }
}

fn select_status(evidence: &[CandidateEvidence]) -> NativeHudStatus {
    if evidence.is_empty() {
        NativeHudStatus::Unavailable
    } else if evidence.iter().any(|candidate| candidate.valid) {
        NativeHudStatus::TraceReady
    } else {
        NativeHudStatus::Rejected
    }
}

#[cfg(test)]
mod tests {
    use super::{CandidateEvidence, NativeHudStatus, select_status};

    #[test]
    fn no_candidate_evidence_reports_unavailable() {
        assert_eq!(select_status(&[]), NativeHudStatus::Unavailable);
    }

    #[test]
    fn a_validated_candidate_reports_trace_ready() {
        let evidence = CandidateEvidence::validated("hud.version.draw", 0x1400_0100_0);
        assert_eq!(select_status(&[evidence]), NativeHudStatus::TraceReady);
    }
}
