//! BMCBL-owned virtual Mod session. Never scans plugin directories or grants
//! arbitrary LoadLibrary/get-proc permissions to the remote plugin.
mod loader;
mod protocol;
mod transport;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;

use crate::bootstrap::runtime_ready::{self, ReadyLevel};
use crate::platform::logging;
use loader::{Loaded, NativeModule};
use protocol::{LaunchPlan, ModuleSpec, RpcCancel, RpcRequest, Shutdown, Welcome};
use transport::Connection;

static SESSION: OnceLock<Arc<Session>> = OnceLock::new();
const CONNECT_BUDGET: Duration = Duration::from_secs(2);
const READY_BUDGET: Duration = Duration::from_secs(180);
const MAX_TOTAL_INFLIGHT: usize = 256;

pub struct Session {
    id: String,
    writer: Mutex<Connection>,
    next_seq: AtomicU64,
    event_id: AtomicU64,
    inflight: AtomicUsize,
    closed: AtomicBool,
    max_frame: usize,
    plugins_root: PathBuf,
    plan: Vec<ModuleSpec>,
    loaded: Mutex<HashMap<String, Arc<NativeModule>>>,
}

pub fn initialize() {
    if SESSION.get().is_some() {
        return;
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    if thread::Builder::new().name("bloader-virtual-mod-handshake".to_string())
        .spawn(move || { let _ = sender.send(negotiate()); }).is_err()
    {
        logging::scoped_warn_message("virtual-mods", "cannot start session negotiation worker");
        return;
    }
    match receiver.recv_timeout(CONNECT_BUDGET) {
        Ok(Ok(Some((session, reader)))) => {
            let _ = SESSION.set(session.clone());
            logging::scoped_info_message("virtual-mods",
                &format!("authenticated session ready: modules={}", session.plan.len()));
            if let Err(error) = thread::Builder::new().name("bloader-virtual-mod-ipc".to_string())
                .spawn(move || session.read_loop(reader))
            {
                logging::scoped_warn_message("virtual-mods", &format!("transport worker unavailable: {error}"));
            }
        }
        Ok(Ok(None)) => {} // The launcher did not opt this process into virtual Mod loading.
        Ok(Err(error)) => {
            logging::scoped_warn_message("virtual-mods", &format!("session rejected: {error}"));
        }
        Err(_) => {
            logging::scoped_warn_message("virtual-mods", "session negotiation timed out; skipping virtual Mods");
        }
    }
}

fn negotiate() -> Result<Option<(Arc<Session>, Connection)>, String> {
    let Some(connection) = transport::connect()? else { return Ok(None) };
    let mut stream = connection.stream;
    let target_pid = std::process::id();
    protocol::write_frame(&mut stream, &protocol::Hello {
        message_type: "hello",
        seq: protocol::sequence(1),
        protocol_major: protocol::MAJOR,
        protocol_minor: protocol::MINOR,
        target_pid,
        transport: connection.transport,
        max_frame_bytes: protocol::MAX_FRAME,
        phases: ["preload", "ready"],
        capabilities: ["module_abi_v1", "rpc_v1"],
    }, protocol::MAX_FRAME)?;
    let welcome_bytes = protocol::read_frame(&mut stream, protocol::MAX_FRAME)?;
    let welcome: Welcome = protocol::decode(&welcome_bytes)?;
    if welcome.message_type != "welcome" || welcome.protocol_major != protocol::MAJOR
        || welcome.protocol_minor > protocol::MINOR
        || !protocol::valid_uuid(&welcome.session_id)
        || welcome.max_frame_bytes < 256 || welcome.max_frame_bytes > protocol::MAX_FRAME
        || welcome.game_version.len() > 64 || welcome.game_version.is_empty()
        || welcome.game_instance_id.len() > 128
    {
        return Err("PROTOCOL_MISMATCH: invalid host welcome".into());
    }
    let mut incoming = protocol::parse_sequence(&welcome.seq, 0)?;
    let plan_bytes = protocol::read_frame(&mut stream, welcome.max_frame_bytes)?;
    let plan: LaunchPlan = protocol::decode(&plan_bytes)?;
    incoming = protocol::check_plan(&plan, &welcome.session_id, incoming)?;
    let accepted = plan.modules.iter().map(ModuleSpec::identifier).collect::<Vec<_>>();
    let reader = stream.try_clone()?;
    let session = Arc::new(Session {
        id: welcome.session_id,
        writer: Mutex::new(stream),
        next_seq: AtomicU64::new(2), // hello was first outgoing packet
        event_id: AtomicU64::new(1),
        inflight: AtomicUsize::new(0),
        closed: AtomicBool::new(false),
        max_frame: welcome.max_frame_bytes,
        plugins_root: PathBuf::from(plan.plugins_root),
        plan: plan.modules,
        loaded: Mutex::new(HashMap::new()),
    });
    session.write(&protocol::PlanAck {
        message_type: "plan_ack",
        seq: session.next_sequence(),
        session_id: &session.id,
        accepted,
        rejected: Vec::new(),
    })?;
    // Only a single read worker owns inbound seq numbers after the launch plan.
    session.incoming_initial(incoming);
    Ok(Some((session, reader)))
}

impl Session {
    fn incoming_initial(&self, _seq: u64) {} // The plan is always seq=2 in v1.

    fn next_sequence(&self) -> String {
        protocol::sequence(self.next_seq.fetch_add(1, Ordering::AcqRel))
    }

    fn write<T: serde::Serialize>(&self, message: &T) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("SESSION_CLOSED".into());
        }
        let mut stream = self.writer.lock().unwrap_or_else(|error| error.into_inner());
        protocol::write_frame(&mut *stream, message, self.max_frame)
    }

    fn status(&self, module: &ModuleSpec, state: &'static str,
              rpc_available: bool, error_code: Option<&'static str>) {
        let id = module.identifier();
        let _ = self.write(&protocol::ModuleStatus {
            message_type: "module_status",
            seq: self.next_sequence(),
            session_id: &self.id,
            module: &id,
            state,
            phase: &module.phase,
            rpc_available,
            error_code,
        });
    }

    fn load_module(self: &Arc<Self>, module: &ModuleSpec, allowed: bool) {
        if !allowed {
            self.status(module, "skipped", false, Some("PHASE_UNSUPPORTED"));
            return;
        }
        if self.closed.load(Ordering::Acquire) {
            self.status(module, "skipped", false, Some("SESSION_CLOSED"));
            return;
        }
        self.status(module, "validating", false, None);
        match loader::load(module, &self.plugins_root, self) {
            Ok(Loaded::NoRpc) => self.status(module, "loaded_no_rpc", false, None),
            Ok(Loaded::Rpc(runtime)) => {
                self.loaded.lock().unwrap_or_else(|error| error.into_inner())
                    .insert(module.identifier(), runtime);
                self.status(module, "loaded", true, None);
            }
            Err(reason) => {
                let state = match reason {
                    "HASH_MISMATCH" | "FILE_UNSAFE" | "DEPENDENCY_UNSAFE" => "verify_failed",
                    "INVALID_PLAN" => "rejected",
                    "ABI_UNSUPPORTED" | "LOAD_FAILED" => "load_failed",
                    _ => "load_failed",
                };
                self.status(module, state, false, Some(reason));
            }
        }
    }

    fn read_loop(self: Arc<Self>, mut reader: Connection) {
        let mut incoming_seq = 2_u64;
        loop {
            let frame = match protocol::read_frame(&mut reader, self.max_frame) {
                Ok(frame) => frame,
                Err(error) => {
                    logging::scoped_debug_message("virtual-mods", &format!("transport closed: {error}"));
                    break;
                }
            };
            let result = match protocol::message_type(&frame).as_deref() {
                Ok("rpc_request") => self.handle_request(&frame, &mut incoming_seq),
                Ok("rpc_cancel") => self.handle_cancel(&frame, &mut incoming_seq),
                Ok("shutdown") => {
                    let parsed: Result<Shutdown, String> = protocol::decode(&frame);
                    match parsed {
                        Ok(message) if message.message_type == "shutdown" && message.session_id == self.id
                            && protocol::parse_sequence(&message.seq, incoming_seq).is_ok() => break,
                        _ => Err("INVALID_MESSAGE: invalid shutdown".into()),
                    }
                }
                _ => Err("INVALID_MESSAGE: unknown inbound message type".into()),
            };
            if let Err(error) = result {
                logging::scoped_warn_message("virtual-mods", &format!("protocol rejected: {error}"));
                break;
            }
        }
        self.closed.store(true, Ordering::Release);
        let modules = self.loaded.lock().unwrap_or_else(|error| error.into_inner());
        for runtime in modules.values() {
            runtime.pending.lock().unwrap_or_else(|error| error.into_inner()).clear();
        }
        self.inflight.store(0, Ordering::Release);
    }

    fn handle_request(&self, frame: &[u8], incoming_seq: &mut u64) -> Result<(), String> {
        let request: RpcRequest = protocol::decode(frame)?;
        if request.message_type != "rpc_request" || request.session_id != self.id
            || !protocol::valid_method(&request.method)
            || request.body_b64.len() > 44_000
        {
            return Err("INVALID_MESSAGE: invalid rpc request".into());
        }
        *incoming_seq = protocol::parse_sequence(&request.seq, *incoming_seq)?;
        let request_id = protocol::parse_sequence(&request.request_id, 0)?;
        let body = STANDARD.decode(request.body_b64.as_bytes())
            .map_err(|_| "INVALID_MESSAGE: invalid RPC base64")?;
        if body.len() > 32 * 1024 {
            return Err("INVALID_MESSAGE: RPC payload too large".into());
        }
        let module = self.loaded.lock().unwrap_or_else(|error| error.into_inner())
            .get(&request.module).cloned();
        let Some(runtime) = module else {
            self.send_response(&request.module, &request.plugin_generation, request_id,
                               None, Some("MODULE_UNAVAILABLE"));
            return Ok(());
        };
        if runtime.generation != request.plugin_generation {
            self.send_response(&request.module, &request.plugin_generation, request_id,
                               None, Some("GENERATION_EXPIRED"));
            return Ok(());
        }
        if self.inflight.fetch_add(1, Ordering::AcqRel) >= MAX_TOTAL_INFLIGHT {
            self.inflight.fetch_sub(1, Ordering::AcqRel);
            self.send_response(&request.module, &request.plugin_generation, request_id,
                               None, Some("BUSY"));
            return Ok(());
        }
        if let Err(error) = runtime.enqueue(request_id, request.method, body) {
            self.inflight.fetch_sub(1, Ordering::AcqRel);
            self.send_response(&request.module, &request.plugin_generation, request_id,
                               None, Some(error));
        }
        Ok(())
    }

    fn handle_cancel(&self, frame: &[u8], incoming_seq: &mut u64) -> Result<(), String> {
        let message: RpcCancel = protocol::decode(frame)?;
        if message.message_type != "rpc_cancel" || message.session_id != self.id {
            return Err("INVALID_MESSAGE: invalid RPC cancel".into());
        }
        *incoming_seq = protocol::parse_sequence(&message.seq, *incoming_seq)?;
        let request_id = protocol::parse_sequence(&message.request_id, 0)?;
        if let Some(runtime) = self.loaded.lock().unwrap_or_else(|error| error.into_inner())
            .get(&message.module)
            .filter(|runtime| runtime.generation == message.plugin_generation)
            && runtime.cancel(request_id)
        {
            self.finish_inflight();
        }
        Ok(())
    }

    pub(super) fn finish_inflight(&self) {
        let _ = self.inflight.fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |count| count.checked_sub(1));
    }

    pub(super) fn send_response(&self, module: &str, generation: &str, request: u64,
                                bytes: Option<&[u8]>, error_code: Option<&str>) {
        let _ = self.write(&protocol::RpcResponse {
            message_type: "rpc_response",
            seq: self.next_sequence(),
            session_id: &self.id,
            module,
            plugin_generation: generation,
            request_id: protocol::sequence(request),
            ok: error_code.is_none(),
            body_b64: STANDARD.encode(bytes.unwrap_or_default()),
            error_code,
        });
    }

    pub(super) fn send_event(&self, module: &str, generation: &str,
                            name: &str, body_b64: &str) {
        let id = self.event_id.fetch_add(1, Ordering::AcqRel);
        let _ = self.write(&protocol::ModEvent {
            message_type: "mod_event",
            seq: self.next_sequence(),
            session_id: &self.id,
            module,
            plugin_generation: generation,
            event_id: protocol::sequence(id),
            name,
            body_b64: body_b64.to_string(),
        });
    }
}

/// Called after PreLoader initialization, before Minecraft's OEP release.
pub fn load_preloads(execution_mode: &str) {
    let Some(session) = SESSION.get().cloned() else { return };
    for module in &session.plan {
        if module.phase == "preload" {
            session.load_module(module, execution_mode == "oep-gated-startup-thread");
        }
    }
}

/// Loaded on a separate worker only after the game process has been released.
pub fn schedule_ready() {
    let Some(session) = SESSION.get().cloned() else { return };
    if !session.plan.iter().any(|module| module.phase == "ready") {
        return;
    }
    let _ = thread::Builder::new()
        .name("bloader-virtual-mod-ready".to_string())
        .spawn(move || {
            let ready = runtime_ready::wait_for(ReadyLevel::Process, READY_BUDGET);
            for module in &session.plan {
                if module.phase == "ready" {
                    session.load_module(module, ready);
                }
            }
        });
}
