//! Transport boundary. Windows uses a process-bound named pipe. Under Wine/Proton,
//! the Linux host may provide a private one-use descriptor for authenticated loopback.
//! Neither endpoint shares XUser credentials or its IPC namespace.
use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::platform::runtime_environment;

const BOOTSTRAP_ENV: &str = "BMCBL_VMOD_BOOTSTRAP";
const DOMAIN: &[u8] = b"BMCBL-VMOD-IPC-V1";
const CHALLENGE_MAGIC: &[u8; 8] = b"BVMOD1\0\0";
const MAX_BOOTSTRAP_BYTES: u64 = 4096;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcessId() -> u32;
    fn GetNamedPipeServerProcessId(pipe: *mut c_void, server_pid: *mut u32) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
    fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

#[link(name = "bcrypt")]
unsafe extern "system" {
    fn BCryptGenRandom(algorithm: *mut c_void, buffer: *mut u8, length: u32, flags: u32) -> i32;
}

#[repr(C)]
struct ProcessEntry32W {
    size: u32,
    usage: u32,
    process_id: u32,
    default_heap_id: usize,
    module_id: u32,
    threads: u32,
    parent_process_id: u32,
    priority_class_base: i32,
    flags: u32,
    exe_file: [u16; 260],
}

pub enum Connection {
    Pipe(File),
    Loopback(TcpStream),
}

impl Connection {
    pub fn try_clone(&self) -> Result<Self, String> {
        match self {
            Self::Pipe(file) => file.try_clone().map(Self::Pipe),
            Self::Loopback(stream) => stream.try_clone().map(Self::Loopback),
        }.map_err(|error| format!("SESSION_CLOSED: clone stream: {error}"))
    }
}

impl Read for Connection {
    fn read(&mut self, data: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Pipe(pipe) => pipe.read(data),
            Self::Loopback(stream) => stream.read(data),
        }
    }
}

impl Write for Connection {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Pipe(pipe) => pipe.write(data),
            Self::Loopback(stream) => stream.write(data),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Pipe(pipe) => pipe.flush(),
            Self::Loopback(stream) => stream.flush(),
        }
    }
}

pub struct AuthenticatedConnection {
    pub stream: Connection,
    pub transport: &'static str,
}

pub fn connect() -> Result<Option<AuthenticatedConnection>, String> {
    if runtime_environment::prime_detection().is_wine() {
        connect_wine()
    } else {
        connect_windows()
    }
}

fn connect_windows() -> Result<Option<AuthenticatedConnection>, String> {
    let target = unsafe { GetCurrentProcessId() };
    let name = format!(r"\\.\pipe\BMCBL.PluginMods.{target}");
    let stream = match OpenOptions::new().read(true).write(true).open(name) {
        Ok(stream) => stream,
        Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied) => return Ok(None),
        Err(error) => return Err(format!("SESSION_CLOSED: open virtual Mod pipe: {error}")),
    };
    let mut server_pid = 0;
    if unsafe { GetNamedPipeServerProcessId(stream.as_raw_handle().cast(), &mut server_pid) } == 0
        || server_pid == 0 || parent_process_id(target) != Some(server_pid)
    {
        return Err("UNAUTHORIZED_SESSION: pipe server is not the launcher parent".into());
    }
    Ok(Some(AuthenticatedConnection { stream: Connection::Pipe(stream), transport: "named-pipe" }))
}

fn parent_process_id(current: u32) -> Option<u32> {
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() || snapshot as isize == -1 {
        return None;
    }
    let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
    entry.size = std::mem::size_of::<ProcessEntry32W>() as u32;
    let mut result = None;
    if unsafe { Process32FirstW(snapshot, &mut entry) } != 0 {
        loop {
            if entry.process_id == current {
                result = Some(entry.parent_process_id);
                break;
            }
            if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
                break;
            }
        }
    }
    unsafe { CloseHandle(snapshot); }
    result
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinuxBootstrap {
    session_id: String,
    transport: String,
    endpoint: String,
    session_secret: String,
    expires_at: u64,
}

fn connect_wine() -> Result<Option<AuthenticatedConnection>, String> {
    let Some(path) = std::env::var_os(BOOTSTRAP_ENV) else {
        return Ok(None);
    };
    let path = Path::new(&path);
    if !path.is_absolute() {
        return Err("UNAUTHORIZED_SESSION: bootstrap descriptor must be absolute".into());
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| "UNAUTHORIZED_SESSION: missing bootstrap descriptor")?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_BOOTSTRAP_BYTES {
        return Err("UNAUTHORIZED_SESSION: unsafe bootstrap descriptor".into());
    }
    let mut document_bytes = Vec::new();
    File::open(path)
        .map_err(|_| "UNAUTHORIZED_SESSION: cannot open bootstrap descriptor")?
        .take(MAX_BOOTSTRAP_BYTES + 1).read_to_end(&mut document_bytes)
        .map_err(|_| "UNAUTHORIZED_SESSION: cannot read bootstrap descriptor")?;
    if document_bytes.len() as u64 > MAX_BOOTSTRAP_BYTES {
        document_bytes.zeroize();
        return Err("UNAUTHORIZED_SESSION: oversized descriptor".into());
    }
    let document: LinuxBootstrap = serde_json::from_slice(&document_bytes)
        .map_err(|_| "INVALID_MESSAGE: malformed bootstrap descriptor")?;
    document_bytes.zeroize();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    if document.expires_at <= now || document.expires_at.saturating_sub(now) > 120
        || !super::protocol::valid_uuid(&document.session_id)
    {
        return Err("UNAUTHORIZED_SESSION: expired or invalid bootstrap session".into());
    }
    if document.transport != "authenticated-loopback" {
        // AF_UNIX is not enabled until its Winsock support and peer verification
        // are tested against a concrete Wine/Proton build.
        return Err("PROTOCOL_MISMATCH: unsupported Wine transport".into());
    }
    let endpoint: SocketAddr = document.endpoint.parse()
        .map_err(|_| "INVALID_MESSAGE: invalid loopback endpoint")?;
    if endpoint.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
        return Err("UNAUTHORIZED_SESSION: non-loopback endpoint".into());
    }
    let mut secret = STANDARD.decode(document.session_secret.as_bytes())
        .map_err(|_| "UNAUTHORIZED_SESSION: invalid session secret")?;
    if secret.len() != 32 {
        secret.zeroize();
        return Err("UNAUTHORIZED_SESSION: invalid session secret length".into());
    }
    let mut stream = TcpStream::connect_timeout(&endpoint, Duration::from_millis(500))
        .map_err(|_| "SESSION_CLOSED: cannot reach the authenticated host")?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "SESSION_CLOSED: configure receive deadline")?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "SESSION_CLOSED: configure send deadline")?;

    let result = authenticate_loopback(&mut stream, &document.session_id, &secret);
    secret.zeroize();
    result?;
    Ok(Some(AuthenticatedConnection {
        stream: Connection::Loopback(stream),
        transport: "authenticated-loopback",
    }))
}

/// Binary handshake v1: client magic|nonce, server magic|nonce,
/// client HMAC("loader"), server HMAC("host"). HMAC binds session ID
/// and both challenges, with explicit fixed field lengths.
fn authenticate_loopback(stream: &mut TcpStream, session: &str, key: &[u8]) -> Result<(), String> {
    let mut client_nonce = [0_u8; 32];
    if unsafe { BCryptGenRandom(std::ptr::null_mut(), client_nonce.as_mut_ptr(), 32, 0x2) } != 0 {
        return Err("INTERNAL_ERROR: system RNG unavailable".into());
    }
    stream.write_all(CHALLENGE_MAGIC).map_err(|_| "SESSION_CLOSED: send challenge")?;
    stream.write_all(&client_nonce).map_err(|_| "SESSION_CLOSED: send nonce")?;
    let mut magic = [0_u8; 8];
    let mut host_nonce = [0_u8; 32];
    stream.read_exact(&mut magic).map_err(|_| "SESSION_CLOSED: receive challenge")?;
    stream.read_exact(&mut host_nonce).map_err(|_| "SESSION_CLOSED: receive nonce")?;
    if &magic != CHALLENGE_MAGIC {
        return Err("UNAUTHORIZED_SESSION: handshake magic mismatch".into());
    }
    let loader = mac(key, session, &client_nonce, &host_nonce, b"loader");
    stream.write_all(&loader).map_err(|_| "SESSION_CLOSED: send loader proof")?;
    let mut expected = [0_u8; 32];
    stream.read_exact(&mut expected).map_err(|_| "SESSION_CLOSED: receive host proof")?;
    let host = mac(key, session, &client_nonce, &host_nonce, b"host");
    let mut difference = 0_u8;
    for (left, right) in expected.iter().zip(host.iter()) {
        difference |= left ^ right;
    }
    client_nonce.zeroize();
    host_nonce.zeroize();
    expected.zeroize();
    if difference != 0 {
        return Err("UNAUTHORIZED_SESSION: host proof mismatch".into());
    }
    Ok(())
}

fn mac(key: &[u8], session: &str, client: &[u8; 32], server: &[u8; 32], role: &[u8]) -> [u8; 32] {
    let mut inner_pad = [0x36_u8; 64];
    let mut outer_pad = [0x5c_u8; 64];
    for (index, byte) in key.iter().enumerate() {
        inner_pad[index] ^= *byte;
        outer_pad[index] ^= *byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(DOMAIN);
    inner.update(session.as_bytes());
    inner.update(client);
    inner.update(server);
    inner.update(role);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    let digest: [u8; 32] = outer.finalize().into();
    inner_pad.zeroize();
    outer_pad.zeroize();
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handshake_roles_have_different_proofs() {
        let key = [42u8; 32];
        let nonce = [33u8; 32];
        assert_ne!(mac(&key, "session", &nonce, &nonce, b"host"),
                   mac(&key, "session", &nonce, &nonce, b"loader"));
    }
}
