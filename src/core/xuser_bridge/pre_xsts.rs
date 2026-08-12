// SPDX-License-Identifier: GPL-3.0-only

use core::ffi::c_void;
use minhook::MinHook;
use sha2::{Digest as _, Sha256};
use std::{
    mem, ptr,
    sync::{
        OnceLock,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
};

use super::super::{bridge_info, bridge_warn, session};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Default)]
pub struct DiscoverySummary {
    pub user_tokens_markers: usize,
    pub device_token_markers: usize,
    pub title_token_markers: usize,
    pub xsts_markers: usize,
    pub user_tokens_xrefs: usize,
    pub user_tokens_function_candidates: usize,
    pub high_confidence_builder_candidates: usize,
}

#[derive(Clone, Copy)]
struct Section {
    rva: usize,
    size: usize,
    executable: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RuntimeFunction {
    begin_address: u32,
    end_address: u32,
    unwind_info_address: u32,
}

#[repr(C)]
struct MemoryBasicInformation {
    base_address: *mut c_void,
    allocation_base: *mut c_void,
    allocation_protect: u32,
    partition_id: u16,
    region_size: usize,
    state: u32,
    protect: u32,
    type_: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FunctionBounds {
    begin: usize,
    end: usize,
    unwind: usize,
}

type BuilderProbeFn =
    unsafe extern "system" fn(usize, usize, usize, usize, usize, usize, usize, usize) -> usize;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
    fn GetCurrentThreadId() -> u32;
    fn GetCurrentProcess() -> *mut c_void;
    fn ReadProcessMemory(
        process: *mut c_void,
        base_address: *const c_void,
        buffer: *mut c_void,
        size: usize,
        bytes_read: *mut usize,
    ) -> i32;
    fn VirtualQuery(
        address: *const c_void,
        buffer: *mut MemoryBasicInformation,
        length: usize,
    ) -> usize;
    fn RtlLookupFunctionEntry(
        control_pc: u64,
        image_base: *mut u64,
        history_table: *mut c_void,
    ) -> *const RuntimeFunction;
}

static DISCOVERY: OnceLock<Result<DiscoverySummary, String>> = OnceLock::new();
static ORIGINAL_PRE_XSTS_BUILDER: AtomicUsize = AtomicUsize::new(0);
static PRE_XSTS_PROBE_CALLS: AtomicU32 = AtomicU32::new(0);
static PROBE_MODULE_BASE: AtomicUsize = AtomicUsize::new(0);
static PROBE_MODULE_SIZE: AtomicUsize = AtomicUsize::new(0);

const MAX_PROBE_LOG_CALLS: u32 = 16;
const MAX_CALL_TARGET_PROBES: usize = 8;
static ORIGINAL_PRE_XSTS_CALL_TARGETS: [AtomicUsize; MAX_CALL_TARGET_PROBES] =
    [const { AtomicUsize::new(0) }; MAX_CALL_TARGET_PROBES];
static PRE_XSTS_CALL_TARGET_RVAS: [AtomicUsize; MAX_CALL_TARGET_PROBES] =
    [const { AtomicUsize::new(0) }; MAX_CALL_TARGET_PROBES];
static PRE_XSTS_CALL_TARGET_PROBE_CALLS: [AtomicU32; MAX_CALL_TARGET_PROBES] =
    [const { AtomicU32::new(0) }; MAX_CALL_TARGET_PROBES];

const ABI_UNRESOLVED: usize = usize::MAX;
const MAX_SERIALIZED_XSTS_BYTES: usize = 64 * 1024;
static SERIALIZED_XSTS_ABI_SLOT: AtomicUsize = AtomicUsize::new(ABI_UNRESOLVED);
static SERIALIZED_XSTS_ABI_JSON_ARG: AtomicUsize = AtomicUsize::new(ABI_UNRESOLVED);
static SERIALIZED_XSTS_ABI_LEN_ARG: AtomicUsize = AtomicUsize::new(ABI_UNRESOLVED);

pub fn ensure_discovered() -> Result<DiscoverySummary, String> {
    DISCOVERY
        .get_or_init(|| unsafe { discover() })
        .as_ref()
        .copied()
        .map_err(Clone::clone)
}

/// True only after a real Microsoft Runtime call has exposed a serialized
/// pre-XSTS JSON document and BLoader has successfully substituted B's
/// `UserTokens` in a transparent helper invocation that returned normally.
pub fn custom_user_injection_ready() -> bool {
    SERIALIZED_XSTS_ABI_SLOT.load(Ordering::Acquire) != ABI_UNRESOLVED
}

unsafe fn discover() -> Result<DiscoverySummary, String> {
    let module_name = wide("xgameruntime.dll");
    let module = unsafe { GetModuleHandleW(module_name.as_ptr()) };
    if module.is_null() {
        return Err("xgameruntime.dll is not loaded".to_string());
    }

    let base = module as usize;
    let (image_size, sections) = unsafe { parse_pe_sections(base) }?;
    let executable = sections
        .iter()
        .copied()
        .filter(|section| section.executable)
        .collect::<Vec<_>>();
    let module_hash = unsafe { mapped_sections_sha256_prefix(base, &sections) };

    PROBE_MODULE_BASE.store(base, Ordering::Release);
    PROBE_MODULE_SIZE.store(image_size, Ordering::Release);

    bridge_info(&format!(
        "开始定位 Microsoft Runtime pre-XSTS 聚合候选 | module=xgameruntime.dll | image_size=0x{image_size:X} | readable_sections={} | executable_sections={} | mapped_sections_sha256_prefix={module_hash} | secrets_logged=false",
        sections.len(),
        executable.len(),
    ));

    let user_tokens = unsafe { locate_marker(base, &sections, &executable, b"UserTokens") };
    let device_token = unsafe { locate_marker(base, &sections, &executable, b"DeviceToken") };
    let title_token = unsafe { locate_marker(base, &sections, &executable, b"TitleToken") };
    let xsts_authorize = unsafe { locate_marker(base, &sections, &executable, b"xsts/authorize") };
    let xsts_host =
        unsafe { locate_marker(base, &sections, &executable, b"xsts.auth.xboxlive.com") };

    log_marker(base, "UserTokens", &user_tokens);
    log_marker(base, "DeviceToken", &device_token);
    log_marker(base, "TitleToken", &title_token);
    log_marker(base, "xsts/authorize", &xsts_authorize);
    log_marker(base, "xsts.auth.xboxlive.com", &xsts_host);

    let (function_candidates, high_confidence_candidates, probe_candidate) = unsafe {
        log_user_tokens_function_candidates(
            base,
            image_size,
            &user_tokens,
            &device_token,
            &title_token,
            &xsts_authorize,
            &xsts_host,
        )
    };

    if let Some(candidate) = probe_candidate {
        unsafe { install_builder_probe(base, image_size, candidate) };
    }

    let summary = DiscoverySummary {
        user_tokens_markers: user_tokens.addresses.len(),
        device_token_markers: device_token.addresses.len(),
        title_token_markers: title_token.addresses.len(),
        xsts_markers: xsts_authorize.addresses.len() + xsts_host.addresses.len(),
        user_tokens_xrefs: user_tokens.xrefs.len(),
        user_tokens_function_candidates: function_candidates,
        high_confidence_builder_candidates: high_confidence_candidates,
    };

    if summary.user_tokens_markers == 0 {
        bridge_warn(
            "Microsoft Runtime 当前映像未发现明文 UserTokens 标记；真实 XSTS builder 可能使用非 JSON 编码、动态字符串或位于进程外 Gaming Services",
        );
    } else if summary.user_tokens_xrefs == 0 {
        bridge_warn(
            "Microsoft Runtime 已发现 UserTokens 标记但未找到直接 RIP-relative LEA 引用；需要继续沿间接引用/调用图定位 pre-XSTS builder",
        );
    } else if summary.user_tokens_function_candidates == 0 {
        bridge_warn(
            "Microsoft Runtime 已发现 UserTokens 代码引用，但 Windows unwind function table 无法解析其函数边界；该引用可能位于 leaf/thunk 代码，需要改用调用方回溯定位",
        );
    } else {
        bridge_info(&format!(
            "Microsoft Runtime pre-XSTS builder 函数边界已解析 | user_tokens_markers={} | user_tokens_text_xrefs={} | function_candidates={} | high_confidence_candidates={} | mapped_sections_sha256_prefix={module_hash} | next=resolve-builder-call-abi",
            summary.user_tokens_markers,
            summary.user_tokens_xrefs,
            summary.user_tokens_function_candidates,
            summary.high_confidence_builder_candidates,
        ));
    }

    Ok(summary)
}

struct MarkerLocations {
    addresses: Vec<usize>,
    xrefs: Vec<usize>,
}

unsafe fn locate_marker(
    base: usize,
    sections: &[Section],
    executable: &[Section],
    marker: &[u8],
) -> MarkerLocations {
    let mut addresses = Vec::new();
    let utf16 = marker
        .iter()
        .flat_map(|byte| [*byte, 0])
        .collect::<Vec<_>>();

    for section in sections {
        let bytes =
            unsafe { core::slice::from_raw_parts((base + section.rva) as *const u8, section.size) };
        for offset in find_all(bytes, marker) {
            addresses.push(base + section.rva + offset);
        }
        for offset in find_all(bytes, &utf16) {
            addresses.push(base + section.rva + offset);
        }
    }
    addresses.sort_unstable();
    addresses.dedup();

    let mut xrefs = Vec::new();
    for target in &addresses {
        for section in executable {
            let bytes = unsafe {
                core::slice::from_raw_parts((base + section.rva) as *const u8, section.size)
            };
            xrefs.extend(find_rip_relative_lea_refs(
                bytes,
                base + section.rva,
                *target,
            ));
        }
    }
    xrefs.sort_unstable();
    xrefs.dedup();

    MarkerLocations { addresses, xrefs }
}

fn log_marker(base: usize, name: &str, locations: &MarkerLocations) {
    let marker_rvas = locations
        .addresses
        .iter()
        .take(16)
        .map(|address| format!("0x{:X}", address.saturating_sub(base)))
        .collect::<Vec<_>>()
        .join(",");
    let xref_rvas = locations
        .xrefs
        .iter()
        .take(32)
        .map(|address| format!("0x{:X}", address.saturating_sub(base)))
        .collect::<Vec<_>>()
        .join(",");
    bridge_info(&format!(
        "pre-XSTS marker scan | marker={name} | occurrences={} | text_xrefs={} | marker_rvas=[{marker_rvas}] | xref_rvas=[{xref_rvas}] | secrets_logged=false",
        locations.addresses.len(),
        locations.xrefs.len(),
    ));
}

unsafe fn log_user_tokens_function_candidates(
    base: usize,
    image_size: usize,
    user_tokens: &MarkerLocations,
    device_token: &MarkerLocations,
    title_token: &MarkerLocations,
    xsts_authorize: &MarkerLocations,
    xsts_host: &MarkerLocations,
) -> (usize, usize, Option<FunctionBounds>) {
    let mut functions = Vec::<FunctionBounds>::new();
    let mut high_confidence = 0usize;
    let mut first_candidate = None;
    let mut first_high_confidence = None;

    for xref in &user_tokens.xrefs {
        let Some(bounds) = (unsafe { lookup_function_bounds(base, *xref) }) else {
            bridge_warn(&format!(
                "pre-XSTS UserTokens xref 无对应 RUNTIME_FUNCTION | xref_rva=0x{:X} | classification=leaf-or-thunk",
                xref.saturating_sub(base),
            ));
            continue;
        };
        if functions.contains(&bounds) {
            continue;
        }
        functions.push(bounds);
        first_candidate.get_or_insert(bounds);

        let device_refs = count_xrefs_in_function(device_token, bounds);
        let title_refs = count_xrefs_in_function(title_token, bounds);
        let authorize_refs = count_xrefs_in_function(xsts_authorize, bounds);
        let host_refs = count_xrefs_in_function(xsts_host, bounds);
        let co_marker_classes = usize::from(device_refs != 0)
            + usize::from(title_refs != 0)
            + usize::from(authorize_refs != 0)
            + usize::from(host_refs != 0);
        if device_refs != 0 && title_refs != 0 {
            high_confidence = high_confidence.saturating_add(1);
            first_high_confidence.get_or_insert(bounds);
        }

        let lea_register = unsafe { rip_lea_destination_register(*xref) }.unwrap_or("unknown");
        let direct_calls_after_xref =
            unsafe { direct_call_candidates(base, image_size, *xref, bounds.end, 160) };
        let direct_calls_in_function = unsafe {
            direct_call_candidates(
                base,
                image_size,
                bounds.begin,
                bounds.end,
                bounds.end - bounds.begin,
            )
        };
        // 0.2.71 defined call-target probes but never actually installed them.
        // Probe calls closest to the UserTokens xref first, then fill remaining
        // slots with unique helpers from the enclosing builder.
        let mut probe_calls = direct_calls_after_xref.clone();
        for candidate in &direct_calls_in_function {
            if !probe_calls.iter().any(|(_, target)| *target == candidate.1) {
                probe_calls.push(*candidate);
            }
        }
        unsafe { install_call_target_probes(base, image_size, &probe_calls) };

        let after_xref_summary = format_direct_calls(base, *xref, &direct_calls_after_xref);
        let function_call_summary =
            format_direct_calls(base, bounds.begin, &direct_calls_in_function);
        let function_hash = unsafe { function_sha256_prefix(bounds) };
        let xref_window_hash = unsafe { window_sha256_prefix(*xref, 96, bounds.end) };

        bridge_info(&format!(
            "pre-XSTS UserTokens function candidate | xref_rva=0x{:X} | function_begin_rva=0x{:X} | function_end_rva=0x{:X} | function_size=0x{:X} | unwind_rva=0x{:X} | lea_destination={} | DeviceToken_xrefs_in_function={} | TitleToken_xrefs_in_function={} | xsts_authorize_xrefs_in_function={} | xsts_host_xrefs_in_function={} | co_marker_classes={} | direct_call_candidates_after_xref=[{}] | direct_call_targets_in_function=[{}] | function_sha256_prefix={} | xref_window_sha256_prefix={} | secrets_logged=false",
            xref.saturating_sub(base),
            bounds.begin.saturating_sub(base),
            bounds.end.saturating_sub(base),
            bounds.end.saturating_sub(bounds.begin),
            bounds.unwind.saturating_sub(base),
            lea_register,
            device_refs,
            title_refs,
            authorize_refs,
            host_refs,
            co_marker_classes,
            after_xref_summary,
            function_call_summary,
            function_hash,
            xref_window_hash,
        ));
    }

    (
        functions.len(),
        high_confidence,
        first_high_confidence.or(first_candidate),
    )
}

unsafe fn install_call_target_probes(base: usize, image_size: usize, calls: &[(usize, usize)]) {
    let mut installed = 0usize;
    let mut seen_targets = Vec::<usize>::new();
    for (source_call, target) in calls.iter().copied() {
        if seen_targets.contains(&target) {
            continue;
        }
        seen_targets.push(target);
        if installed >= MAX_CALL_TARGET_PROBES {
            break;
        }
        if target < base || target >= base.saturating_add(image_size) {
            continue;
        }
        let slot = installed;
        if ORIGINAL_PRE_XSTS_CALL_TARGETS[slot].load(Ordering::Acquire) != 0 {
            installed = installed.saturating_add(1);
            continue;
        }
        let Some(detour) = call_target_detour(slot) else {
            break;
        };
        let trampoline = match unsafe { MinHook::create_hook(target as *mut c_void, detour) } {
            Ok(value) => value,
            Err(error) => {
                bridge_warn(&format!(
                    "pre-XSTS call target 探针安装失败；继续观察其他候选 | slot={slot} | source_call_rva=0x{:X} | target_rva=0x{:X} | error={error:?} | secrets_logged=false",
                    source_call.saturating_sub(base),
                    target.saturating_sub(base),
                ));
                continue;
            }
        };
        match ORIGINAL_PRE_XSTS_CALL_TARGETS[slot].compare_exchange(
            0,
            trampoline as usize,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                PRE_XSTS_CALL_TARGET_RVAS[slot]
                    .store(target.saturating_sub(base), Ordering::Release);
                if let Err(error) = unsafe { MinHook::enable_all_hooks() } {
                    ORIGINAL_PRE_XSTS_CALL_TARGETS[slot].store(0, Ordering::Release);
                    bridge_warn(&format!(
                        "pre-XSTS call target 探针启用失败；继续观察其他候选 | slot={slot} | source_call_rva=0x{:X} | target_rva=0x{:X} | trampoline=0x{:X} | error={error:?} | secrets_logged=false",
                        source_call.saturating_sub(base),
                        target.saturating_sub(base),
                        trampoline as usize,
                    ));
                    continue;
                }
                bridge_info(&format!(
                    "pre-XSTS call target 探针已安装 | slot={slot} | source_call_rva=0x{:X} | target_rva=0x{:X} | trampoline=0x{:X} | mode=transparent-forwarding | max_logged_calls={} | secrets_logged=false",
                    source_call.saturating_sub(base),
                    target.saturating_sub(base),
                    trampoline as usize,
                    MAX_PROBE_LOG_CALLS,
                ));
                installed = installed.saturating_add(1);
            }
            Err(existing) => {
                bridge_info(&format!(
                    "pre-XSTS call target 探针已由其他线程安装 | slot={slot} | existing_trampoline=0x{existing:X} | target_rva=0x{:X} | secrets_logged=false",
                    target.saturating_sub(base),
                ));
            }
        }
    }
    if installed == 0 && !calls.is_empty() {
        bridge_warn(
            "pre-XSTS call target 探针未安装任何候选；将继续只使用 marker/function 边界诊断 | secrets_logged=false",
        );
    }
}

fn call_target_detour(slot: usize) -> Option<*mut c_void> {
    Some(match slot {
        0 => pre_xsts_call_target_probe_0 as *const () as *mut c_void,
        1 => pre_xsts_call_target_probe_1 as *const () as *mut c_void,
        2 => pre_xsts_call_target_probe_2 as *const () as *mut c_void,
        3 => pre_xsts_call_target_probe_3 as *const () as *mut c_void,
        4 => pre_xsts_call_target_probe_4 as *const () as *mut c_void,
        5 => pre_xsts_call_target_probe_5 as *const () as *mut c_void,
        6 => pre_xsts_call_target_probe_6 as *const () as *mut c_void,
        7 => pre_xsts_call_target_probe_7 as *const () as *mut c_void,
        _ => return None,
    })
}

macro_rules! define_call_target_probe {
    ($name:ident, $slot:expr) => {
        unsafe extern "system" fn $name(
            a0: usize,
            a1: usize,
            a2: usize,
            a3: usize,
            a4: usize,
            a5: usize,
            a6: usize,
            a7: usize,
        ) -> usize {
            unsafe { pre_xsts_call_target_probe($slot, a0, a1, a2, a3, a4, a5, a6, a7) }
        }
    };
}
define_call_target_probe!(pre_xsts_call_target_probe_0, 0);
define_call_target_probe!(pre_xsts_call_target_probe_1, 1);
define_call_target_probe!(pre_xsts_call_target_probe_2, 2);
define_call_target_probe!(pre_xsts_call_target_probe_3, 3);
define_call_target_probe!(pre_xsts_call_target_probe_4, 4);
define_call_target_probe!(pre_xsts_call_target_probe_5, 5);
define_call_target_probe!(pre_xsts_call_target_probe_6, 6);
define_call_target_probe!(pre_xsts_call_target_probe_7, 7);

#[allow(clippy::too_many_arguments)]
unsafe fn pre_xsts_call_target_probe(
    slot: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
    arg7: usize,
) -> usize {
    let call_index = PRE_XSTS_CALL_TARGET_PROBE_CALLS[slot].fetch_add(1, Ordering::AcqRel) + 1;
    let target_rva = PRE_XSTS_CALL_TARGET_RVAS[slot].load(Ordering::Acquire);
    let should_log = call_index <= MAX_PROBE_LOG_CALLS;
    if should_log {
        let args = [arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7]
            .iter()
            .enumerate()
            .map(|(index, value)| unsafe { format_probe_arg(index, *value) })
            .collect::<Vec<_>>()
            .join(",");
        let thread_id = unsafe { GetCurrentThreadId() };
        bridge_info(&format!(
            "pre-XSTS call target probe hit | slot={slot} | target_rva=0x{target_rva:X} | call_index={call_index} | thread_id={thread_id} | args=[{args}] | mode=transparent-forwarding | secrets_logged=false"
        ));
    }
    let original_address = ORIGINAL_PRE_XSTS_CALL_TARGETS[slot].load(Ordering::Acquire);
    if original_address == 0 {
        bridge_warn(&format!(
            "pre-XSTS call target trampoline 丢失；返回 0 以避免执行未知路径 | slot={slot} | target_rva=0x{target_rva:X}"
        ));
        return 0;
    }
    let original: BuilderProbeFn = unsafe { mem::transmute(original_address) };
    let original_args = [arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7];
    let prepared = unsafe { prepare_serialized_xsts_call(slot, original_args) };
    let call_args = prepared
        .as_ref()
        .map_or(original_args, |prepared| prepared.args);
    let result = unsafe {
        original(
            call_args[0],
            call_args[1],
            call_args[2],
            call_args[3],
            call_args[4],
            call_args[5],
            call_args[6],
            call_args[7],
        )
    };
    if let Some(prepared) = prepared.as_ref() {
        learn_serialized_xsts_abi(slot, prepared.json_arg, prepared.len_arg);
    }
    if should_log {
        bridge_info(&format!(
            "pre-XSTS call target probe return | slot={slot} | target_rva=0x{target_rva:X} | call_index={call_index} | result=0x{result:X} | result_class={} | secrets_logged=false",
            unsafe { classify_probe_value(result) },
        ));
    }
    result
}

struct PreparedSerializedXstsCall {
    args: [usize; 8],
    _body: Zeroizing<Vec<u8>>,
    json_arg: usize,
    len_arg: Option<usize>,
}

unsafe fn prepare_serialized_xsts_call(
    slot: usize,
    original_args: [usize; 8],
) -> Option<PreparedSerializedXstsCall> {
    let runtime = session()?;
    let custom_utoken = runtime.custom_user_token()?;

    let learned_slot = SERIALIZED_XSTS_ABI_SLOT.load(Ordering::Acquire);
    if learned_slot != ABI_UNRESOLVED && learned_slot != slot {
        return None;
    }

    let learned_arg = SERIALIZED_XSTS_ABI_JSON_ARG.load(Ordering::Acquire);
    let candidates: Vec<usize> = if learned_slot == slot && learned_arg < original_args.len() {
        vec![learned_arg]
    } else {
        (0..original_args.len()).collect()
    };

    for json_arg in candidates {
        let Some(body) = (unsafe { read_serialized_json(original_args[json_arg]) }) else {
            continue;
        };
        if !contains_bytes(&body, br#"\"UserTokens\""#)
            || !contains_bytes(&body, br#"\"DeviceToken\""#)
            || !contains_bytes(&body, br#"\"TitleToken\""#)
        {
            continue;
        }

        let original_len = body.len();
        let Some(mut replaced) = replace_user_tokens_array(&body, custom_utoken) else {
            continue;
        };
        let new_len = replaced.len();
        replaced.push(0);
        let replaced = Zeroizing::new(replaced);

        let mut args = original_args;
        args[json_arg] = replaced.as_ptr() as usize;

        let learned_len_arg = SERIALIZED_XSTS_ABI_LEN_ARG.load(Ordering::Acquire);
        let len_arg = if learned_slot == slot && learned_len_arg < args.len() {
            let old = original_args[learned_len_arg];
            args[learned_len_arg] = if old == original_len.saturating_add(1) {
                new_len.saturating_add(1)
            } else {
                new_len
            };
            Some(learned_len_arg)
        } else {
            find_unique_length_arg(&original_args, json_arg, original_len).map(|index| {
                let old = original_args[index];
                args[index] = if old == original_len.saturating_add(1) {
                    new_len.saturating_add(1)
                } else {
                    new_len
                };
                index
            })
        };

        return Some(PreparedSerializedXstsCall {
            args,
            _body: replaced,
            json_arg,
            len_arg,
        });
    }
    None
}

fn learn_serialized_xsts_abi(slot: usize, json_arg: usize, len_arg: Option<usize>) {
    if SERIALIZED_XSTS_ABI_SLOT
        .compare_exchange(ABI_UNRESOLVED, slot, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        SERIALIZED_XSTS_ABI_JSON_ARG.store(json_arg, Ordering::Release);
        SERIALIZED_XSTS_ABI_LEN_ARG.store(len_arg.unwrap_or(ABI_UNRESOLVED), Ordering::Release);
        bridge_info(&format!(
            "pre-XSTS serialized ABI 已自举解析 | slot={slot} | json_arg={json_arg} | len_arg={} | UserTokens_source=bmcbl-custom-utoken | DeviceToken=preserved-official | TitleToken=preserved-official | native_identity_passthrough=false | secrets_logged=false",
            len_arg.map_or_else(|| "nul-terminated".to_string(), |value| value.to_string()),
        ));
    }
}

unsafe fn read_serialized_json(address: usize) -> Option<Zeroizing<Vec<u8>>> {
    if address < 0x10000 {
        return None;
    }
    let mut info: MemoryBasicInformation = unsafe { mem::zeroed() };
    if unsafe {
        VirtualQuery(
            address as *const c_void,
            &mut info,
            mem::size_of::<MemoryBasicInformation>(),
        )
    } == 0
        || info.state != 0x1000
        || (info.protect & 0x100) != 0
        || !matches!(info.protect & 0xff, 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80)
    {
        return None;
    }

    let region_start = info.base_address as usize;
    let region_end = region_start.checked_add(info.region_size)?;
    if address < region_start || address >= region_end {
        return None;
    }
    let available = region_end
        .saturating_sub(address)
        .min(MAX_SERIALIZED_XSTS_BYTES);
    if available < 2 {
        return None;
    }

    let mut buffer = Zeroizing::new(vec![0u8; available]);
    let mut bytes_read = 0usize;
    if unsafe {
        ReadProcessMemory(
            GetCurrentProcess(),
            address as *const c_void,
            buffer.as_mut_ptr().cast(),
            available,
            &mut bytes_read,
        )
    } == 0
        || bytes_read == 0
    {
        return None;
    }
    buffer.truncate(bytes_read);
    if let Some(nul) = buffer.iter().position(|byte| *byte == 0) {
        buffer.truncate(nul);
    }
    let first = buffer.iter().position(|byte| !byte.is_ascii_whitespace())?;
    if buffer.get(first).copied() != Some(b'{') {
        return None;
    }
    Some(buffer)
}

fn replace_user_tokens_array(body: &[u8], custom_utoken: &str) -> Option<Vec<u8>> {
    let key = br#"\"UserTokens\""#;
    let key_pos = find_bytes(body, key)?;
    let colon = body[key_pos + key.len()..]
        .iter()
        .position(|byte| *byte == b':')?
        + key_pos
        + key.len();
    let open = body[colon + 1..].iter().position(|byte| *byte == b'[')? + colon + 1;
    let close = find_json_array_end(body, open)?;

    let encoded = Zeroizing::new(serde_json::to_string(custom_utoken).ok()?);
    let mut output = Vec::with_capacity(
        body.len()
            .saturating_sub(close.saturating_sub(open + 1))
            .saturating_add(encoded.len()),
    );
    output.extend_from_slice(&body[..open + 1]);
    output.extend_from_slice(encoded.as_bytes());
    output.extend_from_slice(&body[close..]);
    Some(output)
}

fn find_json_array_end(body: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in body.get(open..)?.iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' => depth = depth.saturating_add(1),
            b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_unique_length_arg(args: &[usize; 8], json_arg: usize, length: usize) -> Option<usize> {
    let matches = args
        .iter()
        .enumerate()
        .filter(|(index, value)| {
            *index != json_arg && (**value == length || **value == length.saturating_add(1))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    (matches.len() == 1).then_some(matches[0])
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    find_bytes(haystack, needle).is_some()
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

unsafe fn probe_marker_hint(value: usize) -> Option<String> {
    let direct = unsafe { read_probe_prefix(value, 2048) }?;
    if let Some(marker) = marker_name(&direct) {
        return Some(format!("direct:{marker}"));
    }
    let pointer_bytes = direct.get(..mem::size_of::<usize>() * 8)?;
    for (index, chunk) in pointer_bytes
        .chunks_exact(mem::size_of::<usize>())
        .enumerate()
    {
        let pointer = usize::from_ne_bytes(chunk.try_into().ok()?);
        if let Some(bytes) = unsafe { read_probe_prefix(pointer, 1024) }
            && let Some(marker) = marker_name(&bytes)
        {
            return Some(format!(
                "indirect+0x{:X}:{marker}",
                index * mem::size_of::<usize>()
            ));
        }
    }
    None
}

unsafe fn read_probe_prefix(address: usize, max: usize) -> Option<Vec<u8>> {
    if address < 0x10000 {
        return None;
    }
    let mut info: MemoryBasicInformation = unsafe { mem::zeroed() };
    if unsafe {
        VirtualQuery(
            address as *const c_void,
            &mut info,
            mem::size_of::<MemoryBasicInformation>(),
        )
    } == 0
        || info.state != 0x1000
        || (info.protect & 0x100) != 0
        || !matches!(info.protect & 0xff, 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80)
    {
        return None;
    }
    let region_end = (info.base_address as usize).checked_add(info.region_size)?;
    let size = region_end.saturating_sub(address).min(max);
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0u8; size];
    let mut bytes_read = 0usize;
    if unsafe {
        ReadProcessMemory(
            GetCurrentProcess(),
            address as *const c_void,
            buffer.as_mut_ptr().cast(),
            size,
            &mut bytes_read,
        )
    } == 0
        || bytes_read == 0
    {
        return None;
    }
    buffer.truncate(bytes_read);
    Some(buffer)
}

fn marker_name(bytes: &[u8]) -> Option<&'static str> {
    [
        ("UserTokens", b"UserTokens".as_slice()),
        ("DeviceToken", b"DeviceToken".as_slice()),
        ("TitleToken", b"TitleToken".as_slice()),
    ]
    .into_iter()
    .find_map(|(name, marker)| contains_bytes(bytes, marker).then_some(name))
}

unsafe fn install_builder_probe(base: usize, image_size: usize, bounds: FunctionBounds) {
    if ORIGINAL_PRE_XSTS_BUILDER.load(Ordering::Acquire) != 0 {
        return;
    }
    if bounds.begin < base
        || bounds.end <= bounds.begin
        || bounds.end > base.saturating_add(image_size)
    {
        bridge_warn("pre-XSTS builder 探针候选边界无效；跳过函数级探针安装");
        return;
    }

    PROBE_MODULE_BASE.store(base, Ordering::Release);
    PROBE_MODULE_SIZE.store(image_size, Ordering::Release);
    let target = bounds.begin as *mut c_void;
    let trampoline = match unsafe {
        MinHook::create_hook(target, pre_xsts_builder_probe as *const () as *mut c_void)
    } {
        Ok(value) => value,
        Err(error) => {
            bridge_warn(&format!(
                "pre-XSTS builder 调用探针安装失败；继续 fail-closed | function_begin_rva=0x{:X} | error={error:?}",
                bounds.begin.saturating_sub(base),
            ));
            return;
        }
    };

    match ORIGINAL_PRE_XSTS_BUILDER.compare_exchange(
        0,
        trampoline as usize,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => {
            if let Err(error) = unsafe { MinHook::enable_all_hooks() } {
                ORIGINAL_PRE_XSTS_BUILDER.store(0, Ordering::Release);
                bridge_warn(&format!(
                    "pre-XSTS builder 调用探针启用失败；继续 fail-closed | function_begin_rva=0x{:X} | trampoline=0x{:X} | error={error:?}",
                    bounds.begin.saturating_sub(base),
                    trampoline as usize,
                ));
                return;
            }
            bridge_info(&format!(
                "pre-XSTS builder 调用探针已安装 | function_begin_rva=0x{:X} | function_end_rva=0x{:X} | function_size=0x{:X} | trampoline=0x{:X} | mode=transparent-forwarding | max_logged_calls={} | secrets_logged=false",
                bounds.begin.saturating_sub(base),
                bounds.end.saturating_sub(base),
                bounds.end.saturating_sub(bounds.begin),
                trampoline as usize,
                MAX_PROBE_LOG_CALLS,
            ));
        }
        Err(existing) => {
            bridge_info(&format!(
                "pre-XSTS builder 调用探针已由其他线程安装 | existing_trampoline=0x{existing:X}"
            ));
        }
    }
}

unsafe extern "system" fn pre_xsts_builder_probe(
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
    arg7: usize,
) -> usize {
    let call_index = PRE_XSTS_PROBE_CALLS.fetch_add(1, Ordering::AcqRel) + 1;
    let should_log = call_index <= MAX_PROBE_LOG_CALLS;
    if should_log {
        let args = [arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7]
            .iter()
            .enumerate()
            .map(|(index, value)| unsafe { format_probe_arg(index, *value) })
            .collect::<Vec<_>>()
            .join(",");
        let thread_id = unsafe { GetCurrentThreadId() };
        bridge_info(&format!(
            "pre-XSTS builder probe hit | call_index={call_index} | thread_id={thread_id} | args=[{args}] | mode=transparent-forwarding | secrets_logged=false"
        ));
    }

    let original_address = ORIGINAL_PRE_XSTS_BUILDER.load(Ordering::Acquire);
    if original_address == 0 {
        bridge_warn("pre-XSTS builder trampoline 丢失；返回 0 以避免执行未知路径");
        return 0;
    }
    let original: BuilderProbeFn = unsafe { mem::transmute(original_address) };
    let result = unsafe { original(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7) };

    if should_log {
        bridge_info(&format!(
            "pre-XSTS builder probe return | call_index={call_index} | result=0x{result:X} | result_class={} | secrets_logged=false",
            unsafe { classify_probe_value(result) },
        ));
    }
    result
}

unsafe fn format_probe_arg(index: usize, value: usize) -> String {
    let class = unsafe { classify_probe_value(value) };
    let marker = unsafe { probe_marker_hint(value) };
    match marker {
        Some(marker) => format!("arg{index}=0x{value:X}:{class}:marker_hint={marker}"),
        None => format!("arg{index}=0x{value:X}:{class}"),
    }
}

unsafe fn classify_probe_value(value: usize) -> String {
    if value == 0 {
        return "null".to_string();
    }
    if value < 0x10000 {
        return "small-integer".to_string();
    }

    let base = PROBE_MODULE_BASE.load(Ordering::Acquire);
    let size = PROBE_MODULE_SIZE.load(Ordering::Acquire);
    let in_xgameruntime =
        base != 0 && size != 0 && value >= base && value < base.saturating_add(size);

    let mut info: MemoryBasicInformation = unsafe { mem::zeroed() };
    let queried = unsafe {
        VirtualQuery(
            value as *const c_void,
            &mut info,
            mem::size_of::<MemoryBasicInformation>(),
        )
    };
    if queried == 0 {
        return if in_xgameruntime {
            "unmapped-but-in-xgameruntime-range".to_string()
        } else {
            "unmapped".to_string()
        };
    }

    format!(
        "mapped:state=0x{:X}:protect=0x{:X}:type=0x{:X}:region=0x{:X}:in_xgameruntime={}",
        info.state, info.protect, info.type_, info.region_size, in_xgameruntime,
    )
}

unsafe fn lookup_function_bounds(base: usize, control_pc: usize) -> Option<FunctionBounds> {
    let mut image_base = 0u64;
    let entry =
        unsafe { RtlLookupFunctionEntry(control_pc as u64, &mut image_base, ptr::null_mut()) };
    if entry.is_null() || image_base as usize != base {
        return None;
    }
    let entry = unsafe { entry.read_unaligned() };
    let begin = base.checked_add(entry.begin_address as usize)?;
    let end = base.checked_add(entry.end_address as usize)?;
    let unwind = base.checked_add(entry.unwind_info_address as usize)?;
    (begin < end && control_pc >= begin && control_pc < end).then_some(FunctionBounds {
        begin,
        end,
        unwind,
    })
}

fn count_xrefs_in_function(locations: &MarkerLocations, bounds: FunctionBounds) -> usize {
    locations
        .xrefs
        .iter()
        .filter(|address| **address >= bounds.begin && **address < bounds.end)
        .count()
}

unsafe fn rip_lea_destination_register(address: usize) -> Option<&'static str> {
    let first = unsafe { read_u8(address) };
    let (rex, opcode, modrm) = if (0x40..=0x4f).contains(&first) {
        (first, unsafe { read_u8(address + 1) }, unsafe {
            read_u8(address + 2)
        })
    } else {
        (0, first, unsafe { read_u8(address + 1) })
    };
    if opcode != 0x8d || (modrm & 0xc7) != 0x05 {
        return None;
    }
    let register = ((modrm >> 3) & 0x07) | (((rex >> 2) & 0x01) << 3);
    const REGISTERS: [&str; 16] = [
        "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12",
        "r13", "r14", "r15",
    ];
    REGISTERS.get(register as usize).copied()
}

unsafe fn direct_call_candidates(
    base: usize,
    image_size: usize,
    start: usize,
    end: usize,
    max_bytes: usize,
) -> Vec<(usize, usize)> {
    let end = start.saturating_add(max_bytes).min(end);
    if end <= start || end - start < 5 {
        return Vec::new();
    }
    let code = unsafe { core::slice::from_raw_parts(start as *const u8, end - start) };
    let image_end = base.saturating_add(image_size);
    let mut calls = Vec::new();
    for index in 0..=code.len() - 5 {
        if code[index] != 0xe8 {
            continue;
        }
        let displacement =
            i32::from_le_bytes(code[index + 1..index + 5].try_into().unwrap()) as isize;
        let call = start + index;
        let target = (call + 5).wrapping_add_signed(displacement);
        if target >= base && target < image_end {
            calls.push((call, target));
        }
    }
    calls
}

fn format_direct_calls(base: usize, anchor: usize, calls: &[(usize, usize)]) -> String {
    calls
        .iter()
        .take(16)
        .map(|(call, target)| {
            format!(
                "0x{:X}->0x{:X}(+0x{:X})",
                call.saturating_sub(base),
                target.saturating_sub(base),
                call.saturating_sub(anchor),
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

unsafe fn function_sha256_prefix(bounds: FunctionBounds) -> String {
    let size = bounds.end.saturating_sub(bounds.begin).min(1024 * 1024);
    if size == 0 {
        return "none".to_string();
    }
    let bytes = unsafe { core::slice::from_raw_parts(bounds.begin as *const u8, size) };
    sha256_prefix(bytes)
}

unsafe fn window_sha256_prefix(start: usize, max_bytes: usize, function_end: usize) -> String {
    let end = start.saturating_add(max_bytes).min(function_end);
    if end <= start {
        return "none".to_string();
    }
    let bytes = unsafe { core::slice::from_raw_parts(start as *const u8, end - start) };
    sha256_prefix(bytes)
}

unsafe fn mapped_sections_sha256_prefix(base: usize, sections: &[Section]) -> String {
    let mut hasher = Sha256::new();
    for section in sections {
        hasher.update((section.rva as u64).to_le_bytes());
        hasher.update((section.size as u64).to_le_bytes());
        let bytes =
            unsafe { core::slice::from_raw_parts((base + section.rva) as *const u8, section.size) };
        hasher.update(bytes);
    }
    let digest = hasher.finalize();
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn sha256_prefix(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn find_all(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return Vec::new();
    }
    haystack
        .windows(needle.len())
        .enumerate()
        .filter_map(|(index, value)| (value == needle).then_some(index))
        .collect()
}

fn find_rip_relative_lea_refs(code: &[u8], code_address: usize, target: usize) -> Vec<usize> {
    let mut refs = Vec::new();
    let mut index = 0usize;
    while index + 6 <= code.len() {
        let (opcode_index, instruction_len) = if index + 7 <= code.len()
            && (0x40..=0x4f).contains(&code[index])
            && code[index + 1] == 0x8d
            && (code[index + 2] & 0xc7) == 0x05
        {
            (index + 1, 7usize)
        } else if code[index] == 0x8d && (code[index + 1] & 0xc7) == 0x05 {
            (index, 6usize)
        } else {
            index += 1;
            continue;
        };

        let displacement_offset = opcode_index + 2;
        let displacement = i32::from_le_bytes(
            code[displacement_offset..displacement_offset + 4]
                .try_into()
                .unwrap(),
        ) as isize;
        let next = code_address + index + instruction_len;
        let resolved = next.wrapping_add_signed(displacement);
        if resolved == target {
            refs.push(code_address + index);
        }
        index += instruction_len;
    }
    refs
}

unsafe fn parse_pe_sections(base: usize) -> Result<(usize, Vec<Section>), String> {
    if unsafe { read_u16(base) } != 0x5a4d {
        return Err("xgameruntime.dll DOS header is invalid".to_string());
    }
    let e_lfanew = unsafe { read_u32(base + 0x3c) } as usize;
    if e_lfanew < 0x40 || e_lfanew > 16 * 1024 * 1024 {
        return Err("xgameruntime.dll PE header offset is invalid".to_string());
    }
    let nt = base + e_lfanew;
    if unsafe { read_u32(nt) } != 0x0000_4550 {
        return Err("xgameruntime.dll PE signature is invalid".to_string());
    }

    let section_count = unsafe { read_u16(nt + 6) } as usize;
    let optional_size = unsafe { read_u16(nt + 20) } as usize;
    let optional = nt + 24;
    if section_count == 0 || section_count > 96 || optional_size < 64 {
        return Err("xgameruntime.dll PE section metadata is invalid".to_string());
    }
    let magic = unsafe { read_u16(optional) };
    if magic != 0x20b && magic != 0x10b {
        return Err("xgameruntime.dll optional header is unsupported".to_string());
    }
    let image_size = unsafe { read_u32(optional + 56) } as usize;
    if image_size < 0x1000 || image_size > 1024 * 1024 * 1024 {
        return Err("xgameruntime.dll SizeOfImage is invalid".to_string());
    }

    let section_table = optional + optional_size;
    let mut sections = Vec::with_capacity(section_count);
    for index in 0..section_count {
        let header = section_table + index * 40;
        let virtual_size = unsafe { read_u32(header + 8) } as usize;
        let virtual_address = unsafe { read_u32(header + 12) } as usize;
        let raw_size = unsafe { read_u32(header + 16) } as usize;
        let characteristics = unsafe { read_u32(header + 36) };
        let readable = characteristics & 0x4000_0000 != 0;
        let executable = characteristics & 0x2000_0000 != 0;
        let discardable = characteristics & 0x0200_0000 != 0;
        if discardable || (!readable && !executable) || virtual_address >= image_size {
            continue;
        }
        let requested = virtual_size.max(raw_size);
        let size = requested.min(image_size - virtual_address);
        if size == 0 {
            continue;
        }
        sections.push(Section {
            rva: virtual_address,
            size,
            executable,
        });
    }
    if sections.is_empty() {
        return Err("xgameruntime.dll has no readable mapped PE sections".to_string());
    }
    Ok((image_size, sections))
}

unsafe fn read_u8(address: usize) -> u8 {
    unsafe { (address as *const u8).read_unaligned() }
}

unsafe fn read_u16(address: usize) -> u16 {
    unsafe { (address as *const u16).read_unaligned() }
}

unsafe fn read_u32(address: usize) -> u32 {
    unsafe { (address as *const u32).read_unaligned() }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_all_marker_occurrences() {
        assert_eq!(
            find_all(b"xxUserTokensyyUserTokens", b"UserTokens"),
            vec![2, 14]
        );
    }

    #[test]
    fn finds_x64_rip_relative_lea() {
        let code_address = 0x1000usize;
        let target = 0x1100usize;
        let next = code_address + 7;
        let displacement = (target as isize - next as isize) as i32;
        let mut code = vec![0x48, 0x8d, 0x0d];
        code.extend_from_slice(&displacement.to_le_bytes());
        code.extend_from_slice(&[0x90, 0x90]);
        assert_eq!(
            find_rip_relative_lea_refs(&code, code_address, target),
            vec![0x1000]
        );
    }

    #[test]
    fn direct_call_scanner_resolves_rel32_target() {
        let target_offset = 0x20usize;
        let mut code = vec![0xe8, 0, 0, 0, 0, 0x90, 0x90, 0x90];
        let base = code.as_ptr() as usize;
        let call = base;
        let target = base + target_offset;
        let displacement = (target as isize - (call + 5) as isize) as i32;
        code[1..5].copy_from_slice(&displacement.to_le_bytes());

        let calls =
            unsafe { direct_call_candidates(base, 0x1000, call, call + code.len(), code.len()) };
        assert_eq!(calls, vec![(call, target)]);
    }
}
