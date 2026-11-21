//! Strict, bounded BMCBL virtual Mod wire protocol v1.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};

pub const MAJOR: u32 = 1;
pub const MINOR: u32 = 0;
pub const MAX_FRAME: usize = 65_536;
pub const MAX_MODULES: usize = 256;
pub const MAX_MODULE_BYTES: u64 = 32 * 1024 * 1024;

pub fn read_frame<R: Read>(reader: &mut R, maximum: usize) -> Result<Vec<u8>, String> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).map_err(io_error)?;
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > maximum.min(MAX_FRAME) {
        return Err("INVALID_FRAME: length exceeds negotiated maximum".into());
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).map_err(io_error)?;
    std::str::from_utf8(&payload).map_err(|_| "INVALID_FRAME: invalid UTF-8")?;
    Ok(payload)
}

pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, message: &T, maximum: usize) -> Result<(), String> {
    let encoded = serde_json::to_vec(message).map_err(|_| "INVALID_MESSAGE: serialization failed")?;
    if encoded.is_empty() || encoded.len() > maximum.min(MAX_FRAME) {
        return Err("INVALID_FRAME: outgoing frame exceeds negotiated maximum".into());
    }
    writer.write_all(&(encoded.len() as u32).to_le_bytes()).map_err(io_error)?;
    writer.write_all(&encoded).map_err(io_error)?;
    writer.flush().map_err(io_error)
}

pub fn decode<T: DeserializeOwned>(frame: &[u8]) -> Result<T, String> {
    serde_json::from_slice(frame).map_err(|error| format!("INVALID_MESSAGE: {error}"))
}

pub fn message_type(frame: &[u8]) -> Result<String, String> {
    let object: serde_json::Value = decode(frame)?;
    object.as_object()
        .and_then(|fields| fields.get("type"))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| "INVALID_MESSAGE: missing type".to_string())
}

pub fn parse_sequence(sequence: &str, last: u64) -> Result<u64, String> {
    if sequence.len() != 16 || !sequence.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        return Err("INVALID_MESSAGE: sequence must be 16 lowercase hex digits".into());
    }
    let number = u64::from_str_radix(sequence, 16).map_err(|_| "INVALID_MESSAGE: invalid sequence")?;
    if number == 0 || number <= last {
        return Err("INVALID_MESSAGE: replayed or out-of-order sequence".into());
    }
    Ok(number)
}

pub fn sequence(number: u64) -> String { format!("{number:016x}") }

pub fn valid_id(identifier: &str) -> bool {
    (3..=64).contains(&identifier.len())
        && identifier.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && !identifier.ends_with('-')
        && !identifier.contains("--")
        && identifier.bytes().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')
}

pub fn valid_sha256(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
}

pub fn valid_uuid(value: &str) -> bool {
    value.len() == 36 && value.bytes().enumerate().all(|(i, ch)| {
        if [8, 13, 18, 23].contains(&i) { ch == b'-' }
        else { ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch) }
    })
}

pub fn valid_method(method: &str) -> bool {
    !method.is_empty() && method.len() <= 128 &&
        method.bytes().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'.' | b'_' | b'-'))
}

#[derive(Serialize)]
pub struct Hello {
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub seq: String,
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub target_pid: u32,
    pub transport: &'static str,
    pub max_frame_bytes: usize,
    pub phases: [&'static str; 2],
    pub capabilities: [&'static str; 2],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Welcome {
    #[serde(rename = "type")]
    pub message_type: String,
    pub seq: String,
    pub session_id: String,
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub max_frame_bytes: usize,
    pub game_version: String,
    pub game_instance_id: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleSpec {
    pub plugin_id: String,
    pub module_id: String,
    pub plugin_generation: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub platform: String,
    pub phase: String,
    pub staged_path: String,
}

impl ModuleSpec {
    pub fn identifier(&self) -> String { format!("{}/{}", self.plugin_id, self.module_id) }
    pub fn valid(&self) -> bool {
        valid_id(&self.plugin_id) && valid_id(&self.module_id)
            && valid_uuid(&self.plugin_generation) && valid_sha256(&self.sha256)
            && (1..=MAX_MODULE_BYTES).contains(&self.size_bytes)
            && self.platform == "windows-x86_64"
            && matches!(self.phase.as_str(), "preload" | "ready")
            && self.staged_path.len() <= 2048
            && !self.staged_path.contains('\0')
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchPlan {
    #[serde(rename = "type")]
    pub message_type: String,
    pub seq: String,
    pub session_id: String,
    pub modules: Vec<ModuleSpec>,
}

#[derive(Serialize)]
pub struct PlanAck<'a> {
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub seq: String,
    pub session_id: &'a str,
    pub accepted: Vec<String>,
    pub rejected: Vec<Rejected>,
}

#[derive(Serialize)]
pub struct Rejected {
    pub module: String,
    pub error_code: &'static str,
}

#[derive(Serialize)]
pub struct ModuleStatus<'a> {
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub seq: String,
    pub session_id: &'a str,
    pub module: &'a str,
    pub state: &'a str,
    pub phase: &'a str,
    pub rpc_available: bool,
    pub error_code: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcRequest {
    #[serde(rename = "type")]
    pub message_type: String,
    pub seq: String,
    pub session_id: String,
    pub module: String,
    pub plugin_generation: String,
    pub request_id: String,
    pub method: String,
    pub body_b64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcCancel {
    #[serde(rename = "type")]
    pub message_type: String,
    pub seq: String,
    pub session_id: String,
    pub module: String,
    pub plugin_generation: String,
    pub request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shutdown {
    #[serde(rename = "type")]
    pub message_type: String,
    pub seq: String,
    pub session_id: String,
}

#[derive(Serialize)]
pub struct RpcResponse<'a> {
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub seq: String,
    pub session_id: &'a str,
    pub module: &'a str,
    pub plugin_generation: &'a str,
    pub request_id: String,
    pub ok: bool,
    pub body_b64: String,
    pub error_code: Option<&'a str>,
}

#[derive(Serialize)]
pub struct ModEvent<'a> {
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub seq: String,
    pub session_id: &'a str,
    pub module: &'a str,
    pub plugin_generation: &'a str,
    pub event_id: String,
    pub name: &'a str,
    pub body_b64: String,
}

pub fn check_plan(plan: &LaunchPlan, session_id: &str, last_seq: u64) -> Result<u64, String> {
    use std::collections::{HashMap, HashSet};
    if plan.message_type != "launch_plan" || plan.session_id != session_id || plan.modules.len() > MAX_MODULES {
        return Err("INVALID_PLAN: invalid type, session or module count".into());
    }
    let next = parse_sequence(&plan.seq, last_seq)?;
    let mut ids = HashSet::new();
    let mut per_plugin = HashMap::<&str, usize>::new();
    for module in &plan.modules {
        if !module.valid() || !ids.insert(module.identifier()) {
            return Err("INVALID_PLAN: invalid or repeated module descriptor".into());
        }
        let count = per_plugin.entry(&module.plugin_id).or_default();
        *count += 1;
        if *count > 32 {
            return Err("INVALID_PLAN: more than 32 modules in one plugin".into());
        }
    }
    Ok(next)
}

fn io_error(error: io::Error) -> String { format!("SESSION_CLOSED: {error}") }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_replayed_sequence_and_malformed_ids() {
        assert!(parse_sequence("0000000000000002", 1).is_ok());
        assert!(parse_sequence("0000000000000001", 1).is_err());
        assert!(!valid_id("../escape"));
        assert!(valid_id("example-mod"));
        assert!(!valid_sha256(&"A".repeat(64)));
        assert!(valid_uuid("e64a40d8-47d4-49dc-85bb-68588a40ef1b"));
    }
    #[test]
    fn bounded_frames() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &serde_json::json!({"type":"x"}), MAX_FRAME).unwrap();
        let frame = read_frame(&mut bytes.as_slice(), MAX_FRAME).unwrap();
        assert_eq!(message_type(&frame).unwrap(), "x");
        assert!(read_frame(&mut &[0xff, 0xff, 0xff, 0xff][..], MAX_FRAME).is_err());
    }
}
