// src/core/network_hook.rs
use parking_lot::RwLock;
use std::ffi::c_void;
use std::mem;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};

use aes::Aes256;
use aes::cipher::{BlockDecrypt, KeyInit};
use minhook::MinHook;
use sha2::{Digest, Sha256};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6, SOCKET, WSABUF, ntohs,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::core::{PCSTR, s};

use crate::config::Config;
use crate::runtime::foundation::logging;

static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
static ENABLED: AtomicBool = AtomicBool::new(false);
static LISTEN_PORT: AtomicU16 = AtomicU16::new(19132);
static LOG_HEX_BYTES: AtomicUsize = AtomicUsize::new(0);
static VERBOSE: AtomicBool = AtomicBool::new(false);
static IGNORE_PORTS: RwLock<Vec<u16>> = RwLock::new(Vec::new());

static ORIGINAL_SOCKET: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WSASOCKETW: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WSASOCKETA: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CONNECT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WSACONNECT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_BIND: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_LISTEN: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_ACCEPT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WSAACCEPT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SEND: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_RECV: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SENDTO: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_RECVFROM: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WSASEND: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WSARECV: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WSASENDTO: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WSARECVFROM: AtomicUsize = AtomicUsize::new(0);

type SocketFn = unsafe extern "system" fn(i32, i32, i32) -> SOCKET;
type WsaSocketFn = unsafe extern "system" fn(i32, i32, i32, *mut c_void, u32, u32) -> SOCKET;
type ConnectFn = unsafe extern "system" fn(SOCKET, *const SOCKADDR, i32) -> i32;
type WsaConnectFn = unsafe extern "system" fn(
    SOCKET,
    *const SOCKADDR,
    i32,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut c_void,
) -> i32;
type BindFn = unsafe extern "system" fn(SOCKET, *const SOCKADDR, i32) -> i32;
type ListenFn = unsafe extern "system" fn(SOCKET, i32) -> i32;
type AcceptFn = unsafe extern "system" fn(SOCKET, *mut SOCKADDR, *mut i32) -> SOCKET;
type WsaAcceptFn =
    unsafe extern "system" fn(SOCKET, *mut SOCKADDR, *mut i32, *mut c_void, usize) -> SOCKET;
type SendFn = unsafe extern "system" fn(SOCKET, *const u8, i32, i32) -> i32;
type RecvFn = unsafe extern "system" fn(SOCKET, *mut u8, i32, i32) -> i32;
type SendToFn = unsafe extern "system" fn(SOCKET, *const u8, i32, i32, *const SOCKADDR, i32) -> i32;
type RecvFromFn =
    unsafe extern "system" fn(SOCKET, *mut u8, i32, i32, *mut SOCKADDR, *mut i32) -> i32;
type WsaSendFn = unsafe extern "system" fn(
    SOCKET,
    *const WSABUF,
    u32,
    *mut u32,
    u32,
    *mut c_void,
    *mut c_void,
) -> i32;
type WsaRecvFn = unsafe extern "system" fn(
    SOCKET,
    *const WSABUF,
    u32,
    *mut u32,
    *mut u32,
    *mut c_void,
    *mut c_void,
) -> i32;
type WsaSendToFn = unsafe extern "system" fn(
    SOCKET,
    *const WSABUF,
    u32,
    *mut u32,
    u32,
    *const SOCKADDR,
    i32,
    *mut c_void,
    *mut c_void,
) -> i32;
type WsaRecvFromFn = unsafe extern "system" fn(
    SOCKET,
    *const WSABUF,
    u32,
    *mut u32,
    *mut u32,
    *mut SOCKADDR,
    *mut i32,
    *mut c_void,
    *mut c_void,
) -> i32;

static P2P_REDIRECTION_ENABLED: AtomicBool = AtomicBool::new(false);
static P2P_TARGET_IP: RwLock<String> = RwLock::new(String::new());

pub fn update_config(config: &Config) {
    ENABLED.store(config.enable_network_hooks, Ordering::SeqCst);
    LISTEN_PORT.store(config.network_listen_port, Ordering::SeqCst);
    LOG_HEX_BYTES.store(config.network_log_hex_bytes, Ordering::SeqCst);
    VERBOSE.store(config.network_verbose, Ordering::SeqCst);
    *IGNORE_PORTS.write() = config.network_ignore_ports.clone();

    P2P_REDIRECTION_ENABLED.store(config.enable_p2p_redirection, Ordering::SeqCst);
    *P2P_TARGET_IP.write() = config.p2p_target_ip.clone();
}

unsafe fn redirect_p2p_sockaddr(
    to: *const SOCKADDR,
    tolen: i32,
    temp_storage: &mut SOCKADDR_IN,
) -> (*const SOCKADDR, i32) {
    if !P2P_REDIRECTION_ENABLED.load(Ordering::Relaxed) || to.is_null() {
        return (to, tolen);
    }

    let target_peer = P2P_TARGET_IP.read();
    if target_peer.is_empty() {
        return (to, tolen);
    }

    let Ok(peer_ip) = target_peer.parse::<std::net::Ipv4Addr>() else {
        return (to, tolen);
    };

    if let Some((orig_ip, orig_port)) = parse_sockaddr(to, tolen) {
        if orig_ip == "127.0.0.1" || orig_ip == "0.0.0.0" || orig_ip == target_peer.as_str() {
            return (to, tolen);
        }

        if orig_port == 7551 || orig_port == 19132 || orig_port == 19133 || orig_port > 1024 {
            let octets = peer_ip.octets();
            let mut sin = SOCKADDR_IN::default();
            sin.sin_family = AF_INET;
            sin.sin_port = windows::Win32::Networking::WinSock::htons(orig_port);
            sin.sin_addr.S_un.S_un_b.s_b1 = octets[0];
            sin.sin_addr.S_un.S_un_b.s_b2 = octets[1];
            sin.sin_addr.S_un.S_un_b.s_b3 = octets[2];
            sin.sin_addr.S_un.S_un_b.s_b4 = octets[3];

            *temp_storage = sin;
            logging::info_message(&format!(
                "[net-hook] ★ [Xbox P2P Redirection] Rewrote target WAN IP {}:{} -> EasyTier IP {}:{}",
                orig_ip, orig_port, target_peer, orig_port
            ));
            return (
                temp_storage as *mut SOCKADDR_IN as *const SOCKADDR,
                mem::size_of::<SOCKADDR_IN>() as i32,
            );
        }
    }

    (to, tolen)
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

pub fn is_installed() -> bool {
    HOOK_INSTALLED.load(Ordering::SeqCst)
}

pub fn get_listen_port() -> u16 {
    LISTEN_PORT.load(Ordering::SeqCst)
}

fn hooks_requested(config: &Config) -> bool {
    config.enable_network_hooks || config.enable_p2p_redirection
}

pub fn install(config: &Config) -> bool {
    update_config(config);

    if !hooks_requested(config) {
        if HOOK_INSTALLED.load(Ordering::SeqCst) {
            logging::info_message(
                "[net-hook] Runtime features disabled; existing detours remain pass-through.",
            );
        } else {
            logging::info_message(
                "[net-hook] Hook installation skipped: network hooks and P2P redirection are both disabled.",
            );
        }
        return true;
    }

    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        logging::info_message(&format!(
            "[net-hook] Config updated. Enabled={}, ListenPort={}, IgnorePorts={:?}",
            config.enable_network_hooks, config.network_listen_port, config.network_ignore_ports
        ));
        return true;
    }

    let installed = unsafe { install_net_hooks() };
    if installed == 0 {
        HOOK_INSTALLED.store(false, Ordering::SeqCst);
        logging::warn_message("[net-hook] Failed to install WinSock API hooks.");
        return false;
    }

    if let Err(error) = unsafe { MinHook::enable_all_hooks() } {
        HOOK_INSTALLED.store(false, Ordering::SeqCst);
        logging::warn_message(&format!("[net-hook] Failed to enable hooks: {error:?}"));
        return false;
    }

    logging::info_message(&format!(
        "[net-hook] Successfully installed {installed} WinSock hooks (TCP/UDP/NetherNet 7551). ListenPort={}, Enabled={}, Verbose={}, IgnorePorts={:?}",
        config.network_listen_port,
        config.enable_network_hooks,
        config.network_verbose,
        config.network_ignore_ports
    ));
    true
}

unsafe fn install_net_hooks() -> usize {
    let module_name = windows::core::w!("ws2_32.dll");
    let mut module = GetModuleHandleW(module_name).unwrap_or_default();
    if module.is_invalid() {
        module = LoadLibraryW(module_name).unwrap_or_default();
    }
    if module.is_invalid() {
        logging::warn_message("[net-hook] ws2_32.dll handle invalid.");
        return 0;
    }

    let mut installed = 0;

    installed += hook_ws2_export(
        module,
        s!("socket"),
        "socket",
        detour_socket as *mut c_void,
        &ORIGINAL_SOCKET,
    );
    installed += hook_ws2_export(
        module,
        s!("WSASocketW"),
        "WSASocketW",
        detour_wsa_socket_w as *mut c_void,
        &ORIGINAL_WSASOCKETW,
    );
    installed += hook_ws2_export(
        module,
        s!("WSASocketA"),
        "WSASocketA",
        detour_wsa_socket_a as *mut c_void,
        &ORIGINAL_WSASOCKETA,
    );
    installed += hook_ws2_export(
        module,
        s!("connect"),
        "connect",
        detour_connect as *mut c_void,
        &ORIGINAL_CONNECT,
    );
    installed += hook_ws2_export(
        module,
        s!("WSAConnect"),
        "WSAConnect",
        detour_wsa_connect as *mut c_void,
        &ORIGINAL_WSACONNECT,
    );
    installed += hook_ws2_export(
        module,
        s!("bind"),
        "bind",
        detour_bind as *mut c_void,
        &ORIGINAL_BIND,
    );
    installed += hook_ws2_export(
        module,
        s!("listen"),
        "listen",
        detour_listen as *mut c_void,
        &ORIGINAL_LISTEN,
    );
    installed += hook_ws2_export(
        module,
        s!("accept"),
        "accept",
        detour_accept as *mut c_void,
        &ORIGINAL_ACCEPT,
    );
    installed += hook_ws2_export(
        module,
        s!("WSAAccept"),
        "WSAAccept",
        detour_wsa_accept as *mut c_void,
        &ORIGINAL_WSAACCEPT,
    );
    installed += hook_ws2_export(
        module,
        s!("send"),
        "send",
        detour_send as *mut c_void,
        &ORIGINAL_SEND,
    );
    installed += hook_ws2_export(
        module,
        s!("recv"),
        "recv",
        detour_recv as *mut c_void,
        &ORIGINAL_RECV,
    );
    installed += hook_ws2_export(
        module,
        s!("sendto"),
        "sendto",
        detour_sendto as *mut c_void,
        &ORIGINAL_SENDTO,
    );
    installed += hook_ws2_export(
        module,
        s!("recvfrom"),
        "recvfrom",
        detour_recvfrom as *mut c_void,
        &ORIGINAL_RECVFROM,
    );
    installed += hook_ws2_export(
        module,
        s!("WSASend"),
        "WSASend",
        detour_wsa_send as *mut c_void,
        &ORIGINAL_WSASEND,
    );
    installed += hook_ws2_export(
        module,
        s!("WSARecv"),
        "WSARecv",
        detour_wsa_recv as *mut c_void,
        &ORIGINAL_WSARECV,
    );
    installed += hook_ws2_export(
        module,
        s!("WSASendTo"),
        "WSASendTo",
        detour_wsa_send_to as *mut c_void,
        &ORIGINAL_WSASENDTO,
    );
    installed += hook_ws2_export(
        module,
        s!("WSARecvFrom"),
        "WSARecvFrom",
        detour_wsa_recv_from as *mut c_void,
        &ORIGINAL_WSARECVFROM,
    );

    installed
}

unsafe fn hook_ws2_export(
    module: HMODULE,
    export_name: PCSTR,
    display_name: &str,
    detour: *mut c_void,
    storage: &AtomicUsize,
) -> usize {
    let proc_addr = GetProcAddress(module, export_name);
    let Some(target) = proc_addr else {
        logging::warn_message(&format!("[net-hook] Export not found: {display_name}"));
        return 0;
    };

    let target_ptr = target as *mut c_void;
    match MinHook::create_hook(target_ptr, detour) {
        Ok(original) => {
            storage.store(original as usize, Ordering::Release);
            1
        }
        Err(error) => {
            logging::warn_message(&format!(
                "[net-hook] Failed to hook {display_name}: {error:?}"
            ));
            0
        }
    }
}

unsafe fn parse_sockaddr(addr: *const SOCKADDR, namelen: i32) -> Option<(String, u16)> {
    if addr.is_null() || namelen < 2 {
        return None;
    }
    let family = (*addr).sa_family;
    if family == AF_INET {
        if namelen as usize >= mem::size_of::<SOCKADDR_IN>() {
            let in_addr = *(addr as *const SOCKADDR_IN);
            let port = ntohs(in_addr.sin_port);
            let b = in_addr.sin_addr.S_un.S_un_b;
            let ip = format!("{}.{}.{}.{}", b.s_b1, b.s_b2, b.s_b3, b.s_b4);
            return Some((ip, port));
        }
    } else if family == AF_INET6 {
        if namelen as usize >= mem::size_of::<SOCKADDR_IN6>() {
            let in6_addr = *(addr as *const SOCKADDR_IN6);
            let port = ntohs(in6_addr.sin6_port);
            let raw_bytes = in6_addr.sin6_addr.u.Byte;
            let ip = format!(
                "{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}",
                raw_bytes[0],
                raw_bytes[1],
                raw_bytes[2],
                raw_bytes[3],
                raw_bytes[4],
                raw_bytes[5],
                raw_bytes[6],
                raw_bytes[7],
                raw_bytes[8],
                raw_bytes[9],
                raw_bytes[10],
                raw_bytes[11],
                raw_bytes[12],
                raw_bytes[13],
                raw_bytes[14],
                raw_bytes[15]
            );
            return Some((ip, port));
        }
    }
    None
}

fn should_ignore_port(port: u16) -> bool {
    let ignored = IGNORE_PORTS.read();
    ignored.contains(&port)
}

fn format_socket_type(type_: i32) -> &'static str {
    match type_ {
        1 => "TCP",
        2 => "UDP",
        3 => "RAW",
        _ => "UNKNOWN",
    }
}

fn format_af(af: i32) -> &'static str {
    match af {
        2 => "IPv4",
        23 => "IPv6",
        _ => "AF_OTHER",
    }
}

fn format_hex_dump(buf: *const u8, len: usize, max_bytes: usize) -> String {
    if buf.is_null() || len == 0 || max_bytes == 0 {
        return String::new();
    }
    let dump_len = len.min(max_bytes);
    let slice = unsafe { std::slice::from_raw_parts(buf, dump_len) };
    let hex: Vec<String> = slice.iter().map(|b| format!("{:02X}", b)).collect();
    let suffix = if len > dump_len { "..." } else { "" };
    format!(" | HEX: {}{}", hex.join(" "), suffix)
}

/// Parses NetherNet 7551 LAN Discovery & WebRTC Signaling packets
fn parse_nethernet_packet(buf: &[u8]) -> Option<String> {
    if buf.is_empty() {
        return None;
    }

    // 1. Direct UTF-8 Plaintext check
    if let Ok(text) = std::str::from_utf8(buf) {
        let clean = text.trim_matches('\0').trim();
        if clean.contains("CONNECT")
            || clean.contains("CANDIDATE")
            || clean.contains("MCPE")
            || clean.contains(';')
        {
            return Some(format!("[NetherNet Plaintext] {}", clean));
        }
    }

    // 2. AES-256-ECB Decryption using sha256(0xdeadbeef)
    if buf.len() >= 16 {
        let key_bytes = Sha256::digest(&[0xde, 0xad, 0xbe, 0xef]);
        if let Ok(cipher) = Aes256::new_from_slice(&key_bytes) {
            let mut decrypted = buf.to_vec();
            let block_size = 16;
            let num_blocks = decrypted.len() / block_size;
            for i in 0..num_blocks {
                let block_start = i * block_size;
                let block = aes::cipher::generic_array::GenericArray::from_mut_slice(
                    &mut decrypted[block_start..block_start + block_size],
                );
                cipher.decrypt_block(block);
            }

            if let Ok(text) = std::str::from_utf8(&decrypted) {
                let clean = text.trim_matches('\0').trim();
                if clean.contains("CONNECT")
                    || clean.contains("CANDIDATE")
                    || clean.contains("MCPE")
                    || clean.contains(';')
                {
                    return Some(format!("[NetherNet Decrypted] {}", clean));
                }
            }

            let printable: String = decrypted
                .iter()
                .filter(|&&b| (0x20..=0x7e).contains(&b))
                .map(|&b| b as char)
                .collect();
            if printable.len() >= 8
                && (printable.contains("CONNECT")
                    || printable.contains("MCPE")
                    || printable.contains("CANDIDATE"))
            {
                return Some(format!("[NetherNet Printable] {}", printable));
            }
        }
    }

    // 3. Extract printable ASCII substring
    let printable: String = buf
        .iter()
        .filter(|&&b| (0x20..=0x7e).contains(&b))
        .map(|&b| b as char)
        .collect();
    if printable.len() >= 10
        && (printable.contains("MCPE")
            || printable.contains("CONNECT")
            || printable.contains("CANDIDATE"))
    {
        return Some(format!("[Raw Printable] {}", printable));
    }

    None
}

unsafe extern "system" fn detour_socket(af: i32, type_: i32, protocol: i32) -> SOCKET {
    let original: SocketFn = mem::transmute(ORIGINAL_SOCKET.load(Ordering::Acquire));
    let s = original(af, type_, protocol);

    if ENABLED.load(Ordering::Relaxed) && s.0 != usize::MAX {
        logging::info_message(&format!(
            "[net-hook] SOCKET CREATED handle=0x{:X} family={} proto={} ({})",
            s.0,
            format_af(af),
            format_socket_type(type_),
            protocol
        ));
    }

    s
}

unsafe extern "system" fn detour_wsa_socket_w(
    af: i32,
    type_: i32,
    protocol: i32,
    lp_protocol_info: *mut c_void,
    g: u32,
    dw_flags: u32,
) -> SOCKET {
    let original: WsaSocketFn = mem::transmute(ORIGINAL_WSASOCKETW.load(Ordering::Acquire));
    let s = original(af, type_, protocol, lp_protocol_info, g, dw_flags);

    if ENABLED.load(Ordering::Relaxed) && s.0 != usize::MAX {
        logging::info_message(&format!(
            "[net-hook] WSASocketW CREATED handle=0x{:X} family={} proto={} ({})",
            s.0,
            format_af(af),
            format_socket_type(type_),
            protocol
        ));
    }

    s
}

unsafe extern "system" fn detour_wsa_socket_a(
    af: i32,
    type_: i32,
    protocol: i32,
    lp_protocol_info: *mut c_void,
    g: u32,
    dw_flags: u32,
) -> SOCKET {
    let original: WsaSocketFn = mem::transmute(ORIGINAL_WSASOCKETA.load(Ordering::Acquire));
    let s = original(af, type_, protocol, lp_protocol_info, g, dw_flags);

    if ENABLED.load(Ordering::Relaxed) && s.0 != usize::MAX {
        logging::info_message(&format!(
            "[net-hook] WSASocketA CREATED handle=0x{:X} family={} proto={} ({})",
            s.0,
            format_af(af),
            format_socket_type(type_),
            protocol
        ));
    }

    s
}

unsafe extern "system" fn detour_connect(s: SOCKET, name: *const SOCKADDR, namelen: i32) -> i32 {
    let original: ConnectFn = mem::transmute(ORIGINAL_CONNECT.load(Ordering::Acquire));
    let res = original(s, name, namelen);

    if ENABLED.load(Ordering::Relaxed) {
        if let Some((ip, port)) = parse_sockaddr(name, namelen) {
            if !should_ignore_port(port) {
                let listen_port = LISTEN_PORT.load(Ordering::Relaxed);
                let is_listen_target = port == listen_port || port == 7551;
                let tag = if port == 7551 {
                    " ★NETHERNET-7551"
                } else if is_listen_target {
                    " ★LISTEN-TARGET"
                } else {
                    ""
                };
                logging::info_message(&format!(
                    "[net-hook] CONNECT socket=0x{:X} target={}:{} status={}{}",
                    s.0, ip, port, res, tag
                ));
            }
        } else {
            logging::info_message(&format!(
                "[net-hook] CONNECT socket=0x{:X} status={}",
                s.0, res
            ));
        }
    }

    res
}

unsafe extern "system" fn detour_wsa_connect(
    s: SOCKET,
    name: *const SOCKADDR,
    namelen: i32,
    caller_data: *mut c_void,
    callee_data: *mut c_void,
    sqos: *mut c_void,
    gqos: *mut c_void,
) -> i32 {
    let original: WsaConnectFn = mem::transmute(ORIGINAL_WSACONNECT.load(Ordering::Acquire));
    let res = original(s, name, namelen, caller_data, callee_data, sqos, gqos);

    if ENABLED.load(Ordering::Relaxed) {
        if let Some((ip, port)) = parse_sockaddr(name, namelen) {
            if !should_ignore_port(port) {
                let listen_port = LISTEN_PORT.load(Ordering::Relaxed);
                let is_listen_target = port == listen_port || port == 7551;
                let tag = if port == 7551 {
                    " ★NETHERNET-7551"
                } else if is_listen_target {
                    " ★LISTEN-TARGET"
                } else {
                    ""
                };
                logging::info_message(&format!(
                    "[net-hook] WSAConnect socket=0x{:X} target={}:{} status={}{}",
                    s.0, ip, port, res, tag
                ));
            }
        }
    }

    res
}

unsafe extern "system" fn detour_bind(s: SOCKET, name: *const SOCKADDR, namelen: i32) -> i32 {
    let original: BindFn = mem::transmute(ORIGINAL_BIND.load(Ordering::Acquire));
    let res = original(s, name, namelen);

    if ENABLED.load(Ordering::Relaxed) {
        let listen_port = LISTEN_PORT.load(Ordering::Relaxed);
        if let Some((ip, port)) = parse_sockaddr(name, namelen) {
            let matches_listen = port == listen_port || port == 7551;
            let tag = if port == 7551 {
                " ★NETHERNET-7551-BIND"
            } else if matches_listen {
                " ★LISTEN_PORT_MATCH"
            } else {
                ""
            };
            logging::info_message(&format!(
                "[net-hook] BIND (PORT CREATION) socket=0x{:X} address={}:{} res={}{}",
                s.0, ip, port, res, tag
            ));
        } else {
            logging::info_message(&format!("[net-hook] BIND socket=0x{:X} res={}", s.0, res));
        }
    }

    res
}

unsafe extern "system" fn detour_listen(s: SOCKET, backlog: i32) -> i32 {
    let original: ListenFn = mem::transmute(ORIGINAL_LISTEN.load(Ordering::Acquire));
    let res = original(s, backlog);

    if ENABLED.load(Ordering::Relaxed) {
        let listen_port = LISTEN_PORT.load(Ordering::Relaxed);
        logging::info_message(&format!(
            "[net-hook] LISTEN socket=0x{:X} backlog={} res={} (Configured ListenPort={})",
            s.0, backlog, res, listen_port
        ));
    }

    res
}

unsafe extern "system" fn detour_accept(
    s: SOCKET,
    addr: *mut SOCKADDR,
    addrlen: *mut i32,
) -> SOCKET {
    let original: AcceptFn = mem::transmute(ORIGINAL_ACCEPT.load(Ordering::Acquire));
    let client_socket = original(s, addr, addrlen);

    if ENABLED.load(Ordering::Relaxed) {
        let len = if !addrlen.is_null() { *addrlen } else { 0 };
        if let Some((ip, port)) = parse_sockaddr(addr, len) {
            logging::info_message(&format!(
                "[net-hook] ACCEPT (TCP CLIENT) socket=0x{:X} -> client_socket=0x{:X} client={}:{}",
                s.0, client_socket.0, ip, port
            ));
        } else {
            logging::info_message(&format!(
                "[net-hook] ACCEPT socket=0x{:X} -> client_socket=0x{:X}",
                s.0, client_socket.0
            ));
        }
    }

    client_socket
}

unsafe extern "system" fn detour_wsa_accept(
    s: SOCKET,
    addr: *mut SOCKADDR,
    addrlen: *mut i32,
    fn_condition: *mut c_void,
    dw_callback_data: usize,
) -> SOCKET {
    let original: WsaAcceptFn = mem::transmute(ORIGINAL_WSAACCEPT.load(Ordering::Acquire));
    let client_socket = original(s, addr, addrlen, fn_condition, dw_callback_data);

    if ENABLED.load(Ordering::Relaxed) {
        let len = if !addrlen.is_null() { *addrlen } else { 0 };
        if let Some((ip, port)) = parse_sockaddr(addr, len) {
            logging::info_message(&format!(
                "[net-hook] WSAAccept socket=0x{:X} -> client=0x{:X} client={}:{}",
                s.0, client_socket.0, ip, port
            ));
        }
    }

    client_socket
}

unsafe extern "system" fn detour_send(s: SOCKET, buf: *const u8, len: i32, flags: i32) -> i32 {
    let original: SendFn = mem::transmute(ORIGINAL_SEND.load(Ordering::Acquire));
    let sent = original(s, buf, len, flags);

    if ENABLED.load(Ordering::Relaxed) && sent > 0 {
        let verbose = VERBOSE.load(Ordering::Relaxed);
        let max_hex = LOG_HEX_BYTES.load(Ordering::Relaxed);
        let slice = std::slice::from_raw_parts(buf, sent as usize);

        if let Some(nethernet_info) = parse_nethernet_packet(slice) {
            logging::info_message(&format!(
                "[net-hook] ★ SEND (TCP) socket=0x{:X} | {}",
                s.0, nethernet_info
            ));
        } else if verbose || max_hex > 0 {
            let hex = format_hex_dump(buf, sent as usize, max_hex);
            logging::info_message(&format!(
                "[net-hook] SEND (TCP) socket=0x{:X} bytes={}{}",
                s.0, sent, hex
            ));
        }
    }

    sent
}

unsafe extern "system" fn detour_recv(s: SOCKET, buf: *mut u8, len: i32, flags: i32) -> i32 {
    let original: RecvFn = mem::transmute(ORIGINAL_RECV.load(Ordering::Acquire));
    let recvd = original(s, buf, len, flags);

    if ENABLED.load(Ordering::Relaxed) && recvd > 0 {
        let verbose = VERBOSE.load(Ordering::Relaxed);
        let max_hex = LOG_HEX_BYTES.load(Ordering::Relaxed);
        let slice = std::slice::from_raw_parts(buf, recvd as usize);

        if let Some(nethernet_info) = parse_nethernet_packet(slice) {
            logging::info_message(&format!(
                "[net-hook] ★ RECV (TCP) socket=0x{:X} | {}",
                s.0, nethernet_info
            ));
        } else if verbose || max_hex > 0 {
            let hex = format_hex_dump(buf, recvd as usize, max_hex);
            logging::info_message(&format!(
                "[net-hook] RECV (TCP) socket=0x{:X} bytes={}{}",
                s.0, recvd, hex
            ));
        }
    }

    recvd
}

unsafe extern "system" fn detour_sendto(
    s: SOCKET,
    buf: *const u8,
    len: i32,
    flags: i32,
    to: *const SOCKADDR,
    tolen: i32,
) -> i32 {
    let mut temp_storage = SOCKADDR_IN::default();
    let (final_to, final_tolen) = redirect_p2p_sockaddr(to, tolen, &mut temp_storage);

    let original: SendToFn = mem::transmute(ORIGINAL_SENDTO.load(Ordering::Acquire));
    let sent = original(s, buf, len, flags, final_to, final_tolen);

    if ENABLED.load(Ordering::Relaxed) && sent > 0 {
        let port = parse_sockaddr(to, tolen).map(|(_, p)| p).unwrap_or(0);
        if !should_ignore_port(port) {
            let verbose = VERBOSE.load(Ordering::Relaxed);
            let max_hex = LOG_HEX_BYTES.load(Ordering::Relaxed);
            let listen_port = LISTEN_PORT.load(Ordering::Relaxed);
            let is_listen_target = port == listen_port || port == 7551;
            let target_str = parse_sockaddr(to, tolen)
                .map(|(ip, p)| format!("{}:{}", ip, p))
                .unwrap_or_else(|| "<unknown>".to_string());

            let slice = std::slice::from_raw_parts(buf, sent as usize);
            if let Some(nethernet_info) = parse_nethernet_packet(slice) {
                logging::info_message(&format!(
                    "[net-hook] ★ SENDTO (UDP) socket=0x{:X} target={} | {}",
                    s.0, target_str, nethernet_info
                ));
            } else if verbose || max_hex > 0 || is_listen_target {
                let tag = if port == 7551 {
                    " ★NETHERNET-7551"
                } else if is_listen_target {
                    " ★LISTEN-PORT"
                } else {
                    ""
                };
                let hex = format_hex_dump(buf, sent as usize, max_hex);
                logging::info_message(&format!(
                    "[net-hook] SENDTO (UDP/TCP) socket=0x{:X} bytes={} target={}{}{}",
                    s.0, sent, target_str, tag, hex
                ));
            }
        }
    }

    sent
}

unsafe extern "system" fn detour_recvfrom(
    s: SOCKET,
    buf: *mut u8,
    len: i32,
    flags: i32,
    from: *mut SOCKADDR,
    fromlen: *mut i32,
) -> i32 {
    let original: RecvFromFn = mem::transmute(ORIGINAL_RECVFROM.load(Ordering::Acquire));
    let recvd = original(s, buf, len, flags, from, fromlen);

    if ENABLED.load(Ordering::Relaxed) && recvd > 0 {
        let flen = if !fromlen.is_null() { *fromlen } else { 0 };
        let port = parse_sockaddr(from, flen).map(|(_, p)| p).unwrap_or(0);
        if !should_ignore_port(port) {
            let verbose = VERBOSE.load(Ordering::Relaxed);
            let max_hex = LOG_HEX_BYTES.load(Ordering::Relaxed);
            let listen_port = LISTEN_PORT.load(Ordering::Relaxed);
            let is_listen_src = port == listen_port || port == 7551;
            let src_str = parse_sockaddr(from, flen)
                .map(|(ip, p)| format!("{}:{}", ip, p))
                .unwrap_or_else(|| "<unknown>".to_string());

            let slice = std::slice::from_raw_parts(buf, recvd as usize);
            if let Some(nethernet_info) = parse_nethernet_packet(slice) {
                logging::info_message(&format!(
                    "[net-hook] ★ RECVFROM (UDP) socket=0x{:X} src={} | {}",
                    s.0, src_str, nethernet_info
                ));
            } else if verbose || max_hex > 0 || is_listen_src {
                let tag = if port == 7551 {
                    " ★NETHERNET-7551"
                } else if is_listen_src {
                    " ★LISTEN-PORT"
                } else {
                    ""
                };
                let hex = format_hex_dump(buf, recvd as usize, max_hex);
                logging::info_message(&format!(
                    "[net-hook] RECVFROM (UDP/TCP) socket=0x{:X} bytes={} src={}{}{}",
                    s.0, recvd, src_str, tag, hex
                ));
            }
        }
    }

    recvd
}

unsafe extern "system" fn detour_wsa_send(
    s: SOCKET,
    lp_buffers: *const WSABUF,
    dw_buffer_count: u32,
    lp_number_of_bytes_sent: *mut u32,
    dw_flags: u32,
    lp_overlapped: *mut c_void,
    lp_completion_routine: *mut c_void,
) -> i32 {
    let original: WsaSendFn = mem::transmute(ORIGINAL_WSASEND.load(Ordering::Acquire));
    let res = original(
        s,
        lp_buffers,
        dw_buffer_count,
        lp_number_of_bytes_sent,
        dw_flags,
        lp_overlapped,
        lp_completion_routine,
    );

    if ENABLED.load(Ordering::Relaxed) && res == 0 {
        let verbose = VERBOSE.load(Ordering::Relaxed);
        let max_hex = LOG_HEX_BYTES.load(Ordering::Relaxed);
        let sent = if !lp_number_of_bytes_sent.is_null() {
            *lp_number_of_bytes_sent as usize
        } else {
            0
        };
        let first_ptr = if !lp_buffers.is_null() && dw_buffer_count > 0 {
            (*lp_buffers).buf.as_ptr()
        } else {
            std::ptr::null()
        };

        if !first_ptr.is_null() && sent > 0 {
            let slice = std::slice::from_raw_parts(first_ptr, sent);
            if let Some(nethernet_info) = parse_nethernet_packet(slice) {
                logging::info_message(&format!(
                    "[net-hook] ★ WSASend socket=0x{:X} | {}",
                    s.0, nethernet_info
                ));
            } else if verbose || max_hex > 0 {
                let hex = format_hex_dump(first_ptr, sent, max_hex);
                logging::info_message(&format!(
                    "[net-hook] WSASend socket=0x{:X} bytes={}{}",
                    s.0, sent, hex
                ));
            }
        }
    }

    res
}

unsafe extern "system" fn detour_wsa_recv(
    s: SOCKET,
    lp_buffers: *const WSABUF,
    dw_buffer_count: u32,
    lp_number_of_bytes_recvd: *mut u32,
    lp_flags: *mut u32,
    lp_overlapped: *mut c_void,
    lp_completion_routine: *mut c_void,
) -> i32 {
    let original: WsaRecvFn = mem::transmute(ORIGINAL_WSARECV.load(Ordering::Acquire));
    let res = original(
        s,
        lp_buffers,
        dw_buffer_count,
        lp_number_of_bytes_recvd,
        lp_flags,
        lp_overlapped,
        lp_completion_routine,
    );

    if ENABLED.load(Ordering::Relaxed) && res == 0 {
        let verbose = VERBOSE.load(Ordering::Relaxed);
        let max_hex = LOG_HEX_BYTES.load(Ordering::Relaxed);
        let recvd = if !lp_number_of_bytes_recvd.is_null() {
            *lp_number_of_bytes_recvd as usize
        } else {
            0
        };
        let first_ptr = if !lp_buffers.is_null() && dw_buffer_count > 0 {
            (*lp_buffers).buf.as_ptr()
        } else {
            std::ptr::null()
        };

        if !first_ptr.is_null() && recvd > 0 {
            let slice = std::slice::from_raw_parts(first_ptr, recvd);
            if let Some(nethernet_info) = parse_nethernet_packet(slice) {
                logging::info_message(&format!(
                    "[net-hook] ★ WSARecv socket=0x{:X} | {}",
                    s.0, nethernet_info
                ));
            } else if verbose || max_hex > 0 {
                let hex = format_hex_dump(first_ptr, recvd, max_hex);
                logging::info_message(&format!(
                    "[net-hook] WSARecv socket=0x{:X} bytes={}{}",
                    s.0, recvd, hex
                ));
            }
        }
    }

    res
}

unsafe extern "system" fn detour_wsa_send_to(
    s: SOCKET,
    lp_buffers: *const WSABUF,
    dw_buffer_count: u32,
    lp_number_of_bytes_sent: *mut u32,
    dw_flags: u32,
    lp_to: *const SOCKADDR,
    i_tolen: i32,
    lp_overlapped: *mut c_void,
    lp_completion_routine: *mut c_void,
) -> i32 {
    let mut temp_storage = SOCKADDR_IN::default();
    let (final_to, final_tolen) = redirect_p2p_sockaddr(lp_to, i_tolen, &mut temp_storage);

    let original: WsaSendToFn = mem::transmute(ORIGINAL_WSASENDTO.load(Ordering::Acquire));
    let res = original(
        s,
        lp_buffers,
        dw_buffer_count,
        lp_number_of_bytes_sent,
        dw_flags,
        final_to,
        final_tolen,
        lp_overlapped,
        lp_completion_routine,
    );

    if ENABLED.load(Ordering::Relaxed) && res == 0 {
        let sent_bytes = if !lp_number_of_bytes_sent.is_null() {
            *lp_number_of_bytes_sent as usize
        } else if !lp_buffers.is_null() && dw_buffer_count > 0 {
            (*lp_buffers).len as usize
        } else {
            0
        };

        let port = parse_sockaddr(lp_to, i_tolen).map(|(_, p)| p).unwrap_or(0);
        if !should_ignore_port(port) {
            let verbose = VERBOSE.load(Ordering::Relaxed);
            let max_hex = LOG_HEX_BYTES.load(Ordering::Relaxed);
            let listen_port = LISTEN_PORT.load(Ordering::Relaxed);
            let is_listen_target = port == listen_port || port == 7551;
            let target_str = parse_sockaddr(lp_to, i_tolen)
                .map(|(ip, p)| format!("{}:{}", ip, p))
                .unwrap_or_else(|| "<unknown>".to_string());
            let first_ptr = if !lp_buffers.is_null() && dw_buffer_count > 0 {
                (*lp_buffers).buf.as_ptr()
            } else {
                std::ptr::null()
            };

            if !first_ptr.is_null() && sent_bytes > 0 {
                let slice = std::slice::from_raw_parts(first_ptr, sent_bytes);
                if let Some(nethernet_info) = parse_nethernet_packet(slice) {
                    logging::info_message(&format!(
                        "[net-hook] ★ WSASendTo target={} | {}",
                        target_str, nethernet_info
                    ));
                } else if verbose || max_hex > 0 || is_listen_target {
                    let tag = if port == 7551 {
                        " ★NETHERNET-7551"
                    } else if is_listen_target {
                        " ★LISTEN-PORT"
                    } else {
                        ""
                    };
                    let hex = format_hex_dump(first_ptr, sent_bytes, max_hex);
                    logging::info_message(&format!(
                        "[net-hook] WSASendTo (UDP/TCP) socket=0x{:X} bytes={} target={}{}{}",
                        s.0, sent_bytes, target_str, tag, hex
                    ));
                }
            }
        }
    }

    res
}

unsafe extern "system" fn detour_wsa_recv_from(
    s: SOCKET,
    lp_buffers: *const WSABUF,
    dw_buffer_count: u32,
    lp_number_of_bytes_recvd: *mut u32,
    lp_flags: *mut u32,
    lp_from: *mut SOCKADDR,
    lp_fromlen: *mut i32,
    lp_overlapped: *mut c_void,
    lp_completion_routine: *mut c_void,
) -> i32 {
    let original: WsaRecvFromFn = mem::transmute(ORIGINAL_WSARECVFROM.load(Ordering::Acquire));
    let res = original(
        s,
        lp_buffers,
        dw_buffer_count,
        lp_number_of_bytes_recvd,
        lp_flags,
        lp_from,
        lp_fromlen,
        lp_overlapped,
        lp_completion_routine,
    );

    if ENABLED.load(Ordering::Relaxed) && res == 0 {
        let recvd_bytes = if !lp_number_of_bytes_recvd.is_null() {
            *lp_number_of_bytes_recvd as usize
        } else {
            0
        };

        let flen = if !lp_fromlen.is_null() {
            *lp_fromlen
        } else {
            0
        };
        let port = parse_sockaddr(lp_from, flen).map(|(_, p)| p).unwrap_or(0);
        if !should_ignore_port(port) {
            let verbose = VERBOSE.load(Ordering::Relaxed);
            let max_hex = LOG_HEX_BYTES.load(Ordering::Relaxed);
            let listen_port = LISTEN_PORT.load(Ordering::Relaxed);
            let is_listen_src = port == listen_port || port == 7551;
            let src_str = parse_sockaddr(lp_from, flen)
                .map(|(ip, p)| format!("{}:{}", ip, p))
                .unwrap_or_else(|| "<unknown>".to_string());
            let first_ptr = if !lp_buffers.is_null() && dw_buffer_count > 0 {
                (*lp_buffers).buf.as_ptr()
            } else {
                std::ptr::null()
            };

            if !first_ptr.is_null() && recvd_bytes > 0 {
                let slice = std::slice::from_raw_parts(first_ptr, recvd_bytes);
                if let Some(nethernet_info) = parse_nethernet_packet(slice) {
                    logging::info_message(&format!(
                        "[net-hook] ★ WSARecvFrom src={} | {}",
                        src_str, nethernet_info
                    ));
                } else if verbose || max_hex > 0 || is_listen_src {
                    let tag = if port == 7551 {
                        " ★NETHERNET-7551"
                    } else if is_listen_src {
                        " ★LISTEN-PORT"
                    } else {
                        ""
                    };
                    let hex = format_hex_dump(first_ptr, recvd_bytes, max_hex);
                    logging::info_message(&format!(
                        "[net-hook] WSARecvFrom (UDP/TCP) socket=0x{:X} bytes={} src={}{}{}",
                        s.0, recvd_bytes, src_str, tag, hex
                    ));
                }
            }
        }
    }

    res
}

#[cfg(test)]
mod install_policy_tests {
    use super::*;

    #[test]
    fn disabled_network_features_do_not_request_winsock_hooks() {
        let config = Config::default();
        assert!(!hooks_requested(&config));
    }

    #[test]
    fn network_logging_requests_winsock_hooks() {
        let mut config = Config::default();
        config.enable_network_hooks = true;
        assert!(hooks_requested(&config));
    }

    #[test]
    fn p2p_redirection_requests_winsock_hooks() {
        let mut config = Config::default();
        config.enable_p2p_redirection = true;
        assert!(hooks_requested(&config));
    }
}
