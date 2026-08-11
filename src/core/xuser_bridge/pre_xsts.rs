// SPDX-License-Identifier: GPL-3.0-only

use core::ffi::c_void;
use sha2::{Digest as _, Sha256};
use std::{ptr, sync::OnceLock};

use super::super::{bridge_info, bridge_warn};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FunctionBounds {
    begin: usize,
    end: usize,
    unwind: usize,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
    fn RtlLookupFunctionEntry(
        control_pc: u64,
        image_base: *mut u64,
        history_table: *mut c_void,
    ) -> *const RuntimeFunction;
}

static DISCOVERY: OnceLock<Result<DiscoverySummary, String>> = OnceLock::new();

pub fn ensure_discovered() -> Result<DiscoverySummary, String> {
    DISCOVERY
        .get_or_init(|| unsafe { discover() })
        .as_ref()
        .copied()
        .map_err(Clone::clone)
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

    bridge_info(&format!(
        "开始定位 Microsoft Runtime pre-XSTS 聚合候选 | module=xgameruntime.dll | image_size=0x{image_size:X} | readable_sections={} | executable_sections={} | mapped_sections_sha256_prefix={module_hash} | secrets_logged=false",
        sections.len(),
        executable.len(),
    ));

    let user_tokens = unsafe { locate_marker(base, &sections, &executable, b"UserTokens") };
    let device_token = unsafe { locate_marker(base, &sections, &executable, b"DeviceToken") };
    let title_token = unsafe { locate_marker(base, &sections, &executable, b"TitleToken") };
    let xsts_authorize = unsafe { locate_marker(base, &sections, &executable, b"xsts/authorize") };
    let xsts_host = unsafe {
        locate_marker(
            base,
            &sections,
            &executable,
            b"xsts.auth.xboxlive.com",
        )
    };

    // These are intentionally INFO-level. A non-debug user run must still carry
    // the exact RVAs needed to match a local disassembly and write the next hook.
    log_marker(base, "UserTokens", &user_tokens);
    log_marker(base, "DeviceToken", &device_token);
    log_marker(base, "TitleToken", &title_token);
    log_marker(base, "xsts/authorize", &xsts_authorize);
    log_marker(base, "xsts.auth.xboxlive.com", &xsts_host);

    let (function_candidates, high_confidence_candidates) = unsafe {
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
        let bytes = unsafe {
            core::slice::from_raw_parts((base + section.rva) as *const u8, section.size)
        };
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
) -> (usize, usize) {
    let mut functions = Vec::<FunctionBounds>::new();
    let mut high_confidence = 0usize;

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
        }

        let lea_register = unsafe { rip_lea_destination_register(*xref) }.unwrap_or("unknown");
        let direct_calls_after_xref = unsafe {
            direct_call_candidates(base, image_size, *xref, bounds.end, 160)
        };
        let direct_calls_in_function = unsafe {
            direct_call_candidates(base, image_size, bounds.begin, bounds.end, bounds.end - bounds.begin)
        };
        let after_xref_summary = format_direct_calls(base, *xref, &direct_calls_after_xref);
        let function_call_summary = format_direct_calls(base, bounds.begin, &direct_calls_in_function);
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

    (functions.len(), high_confidence)
}

unsafe fn lookup_function_bounds(base: usize, control_pc: usize) -> Option<FunctionBounds> {
    let mut image_base = 0u64;
    let entry = unsafe {
        RtlLookupFunctionEntry(control_pc as u64, &mut image_base, ptr::null_mut())
    };
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
        (
            first,
            unsafe { read_u8(address + 1) },
            unsafe { read_u8(address + 2) },
        )
    } else {
        (0, first, unsafe { read_u8(address + 1) })
    };
    if opcode != 0x8d || (modrm & 0xc7) != 0x05 {
        return None;
    }
    let register = ((modrm >> 3) & 0x07) | (((rex >> 2) & 0x01) << 3);
    const REGISTERS: [&str; 16] = [
        "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10",
        "r11", "r12", "r13", "r14", "r15",
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
        let displacement = i32::from_le_bytes(code[index + 1..index + 5].try_into().unwrap()) as isize;
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
        let bytes = unsafe {
            core::slice::from_raw_parts((base + section.rva) as *const u8, section.size)
        };
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
        assert_eq!(find_all(b"xxUserTokensyyUserTokens", b"UserTokens"), vec![2, 14]);
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
        assert_eq!(find_rip_relative_lea_refs(&code, code_address, target), vec![0x1000]);
    }

    #[test]
    fn direct_call_scanner_resolves_rel32_target() {
        let base = 0x1000usize;
        let xref = 0x1100usize;
        let target = 0x1200usize;
        let displacement = (target as isize - (xref + 5) as isize) as i32;
        let mut code = vec![0xe8];
        code.extend_from_slice(&displacement.to_le_bytes());
        let calls = unsafe { direct_call_candidates(base, 0x1000, xref, xref + code.len(), code.len()) };
        assert_eq!(calls, vec![(xref, target)]);
    }
}
