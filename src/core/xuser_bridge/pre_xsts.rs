// SPDX-License-Identifier: GPL-3.0-only

use core::ffi::c_void;
use std::sync::OnceLock;

use super::{bridge_debug, bridge_info, bridge_warn};

#[derive(Clone, Copy, Debug, Default)]
pub struct DiscoverySummary {
    pub user_tokens_markers: usize,
    pub device_token_markers: usize,
    pub title_token_markers: usize,
    pub xsts_markers: usize,
    pub user_tokens_xrefs: usize,
}

#[derive(Clone, Copy)]
struct Section {
    rva: usize,
    size: usize,
    executable: bool,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
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

    bridge_info(&format!(
        "开始定位 Microsoft Runtime pre-XSTS 聚合候选 | module=xgameruntime.dll | image_size=0x{image_size:X} | sections={} | executable_sections={} | secrets_logged=false",
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

    log_marker("UserTokens", &user_tokens);
    log_marker("DeviceToken", &device_token);
    log_marker("TitleToken", &title_token);
    log_marker("xsts/authorize", &xsts_authorize);
    log_marker("xsts.auth.xboxlive.com", &xsts_host);

    let summary = DiscoverySummary {
        user_tokens_markers: user_tokens.addresses.len(),
        device_token_markers: device_token.addresses.len(),
        title_token_markers: title_token.addresses.len(),
        xsts_markers: xsts_authorize.addresses.len() + xsts_host.addresses.len(),
        user_tokens_xrefs: user_tokens.xrefs.len(),
    };

    if summary.user_tokens_markers == 0 {
        bridge_warn(
            "Microsoft Runtime 当前映像未发现明文 UserTokens 标记；真实 XSTS builder 可能使用非 JSON 编码、动态字符串或位于进程外 Gaming Services",
        );
    } else if summary.user_tokens_xrefs == 0 {
        bridge_warn(
            "Microsoft Runtime 已发现 UserTokens 标记但未找到直接 RIP-relative LEA 引用；需要继续沿间接引用/调用图定位 pre-XSTS builder",
        );
    } else {
        bridge_info(&format!(
            "Microsoft Runtime pre-XSTS 候选已发现 | user_tokens_markers={} | user_tokens_text_xrefs={} | next=resolve-builder-abi-before-hook",
            summary.user_tokens_markers,
            summary.user_tokens_xrefs,
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

fn log_marker(name: &str, locations: &MarkerLocations) {
    let marker_rvas = locations
        .addresses
        .iter()
        .take(8)
        .map(|address| format!("0x{:X}", rva(*address)))
        .collect::<Vec<_>>()
        .join(",");
    let xref_rvas = locations
        .xrefs
        .iter()
        .take(16)
        .map(|address| format!("0x{:X}", rva(*address)))
        .collect::<Vec<_>>()
        .join(",");
    bridge_debug(&format!(
        "pre-XSTS marker scan | marker={name} | occurrences={} | text_xrefs={} | marker_rvas=[{marker_rvas}] | xref_rvas=[{xref_rvas}]",
        locations.addresses.len(),
        locations.xrefs.len(),
    ));
}

fn rva(address: usize) -> usize {
    let module_name = wide("xgameruntime.dll");
    let module = unsafe { GetModuleHandleW(module_name.as_ptr()) } as usize;
    address.saturating_sub(module)
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
        if virtual_address >= image_size {
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
            executable: characteristics & 0x2000_0000 != 0,
        });
    }
    if sections.is_empty() {
        return Err("xgameruntime.dll has no mapped PE sections".to_string());
    }
    Ok((image_size, sections))
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
}
