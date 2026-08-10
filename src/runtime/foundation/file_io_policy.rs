use std::sync::OnceLock;

use crate::utils;

#[derive(Clone, Debug)]
struct FileIoPolicy {
    host_version: Option<String>,
    writes_allowed: bool,
}

static POLICY: OnceLock<FileIoPolicy> = OnceLock::new();

fn policy() -> &'static FileIoPolicy {
    POLICY.get_or_init(|| {
        let host_version = utils::current_application_version();
        let writes_allowed = !host_version
            .as_deref()
            .map(is_legacy_uwp_version)
            .unwrap_or(false);

        FileIoPolicy {
            host_version,
            writes_allowed,
        }
    })
}

/// Returns whether BLoader may create or modify files in the host process.
///
/// Minecraft Bedrock 1.17.x, 1.18.x and 1.19.x run BLoader under the legacy
/// UWP/AppContainer file-access model. Creating logs/config/crash artifacts in
/// the game directory from those hosts can fail before normal bootstrap has
/// completed, so BLoader deliberately becomes read-only for its own artifacts.
pub fn writes_allowed() -> bool {
    policy().writes_allowed
}

pub fn legacy_uwp_no_write() -> bool {
    !writes_allowed()
}

pub fn host_version() -> Option<&'static str> {
    policy().host_version.as_deref()
}

pub fn mode_label() -> &'static str {
    if legacy_uwp_no_write() {
        "legacy-uwp-no-file-write"
    } else {
        "normal-file-io"
    }
}

fn is_legacy_uwp_version(version: &str) -> bool {
    let mut parts = version.split('.');
    let major = parts.next().and_then(|value| value.parse::<u32>().ok());
    let minor = parts.next().and_then(|value| value.parse::<u32>().ok());

    major == Some(1) && matches!(minor, Some(17 | 18 | 19))
}

#[cfg(test)]
mod tests {
    use super::is_legacy_uwp_version;

    #[test]
    fn detects_legacy_uwp_versions() {
        assert!(is_legacy_uwp_version("1.17.0.2"));
        assert!(is_legacy_uwp_version("1.18.31.0"));
        assert!(is_legacy_uwp_version("1.19.83.1"));
    }

    #[test]
    fn keeps_other_versions_writable() {
        assert!(!is_legacy_uwp_version("1.16.221.0"));
        assert!(!is_legacy_uwp_version("1.20.0.1"));
        assert!(!is_legacy_uwp_version("1.21.100.0"));
        assert!(!is_legacy_uwp_version("unknown"));
    }
}
