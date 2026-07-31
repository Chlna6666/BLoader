use std::sync::OnceLock;
use windows::Win32::System::Diagnostics::Debug::{IMAGE_NT_HEADERS64, IMAGE_SECTION_HEADER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::IMAGE_DOS_HEADER;

pub struct Pattern<'a>(&'a [u8], &'a [bool]);

pub fn scan(signature: &str) -> Option<usize> {
    scan_matches(signature, 1).pop()
}

/// Returns an address only when the pattern has exactly one match in the game image.
pub fn scan_unique(signature: &str) -> Option<usize> {
    let mut matches = scan_matches(signature, 2);
    if matches.len() == 1 {
        matches.pop()
    } else {
        None
    }
}

/// Returns an address only when the pattern has exactly one match in the named PE section.
pub fn scan_unique_in_section(signature: &str, section: &str) -> Option<usize> {
    let (bytes, mask) = parse_signature(signature);
    let (base, size) = section_bounds(section)?;
    let mut matches = scan_pattern_in_range(&bytes, &mask, 2, base, size);
    (matches.len() == 1).then(|| matches.pop()).flatten()
}

pub fn image_bounds() -> Option<(usize, usize)> {
    static MODULE_INFO: OnceLock<(usize, usize)> = OnceLock::new();
    let (base, size) = MODULE_INFO.get_or_init(read_module_info);
    (*base != 0 && *size != 0).then_some((*base, *size))
}

pub fn section_bounds(section: &str) -> Option<(usize, usize)> {
    let (base, image_size) = image_bounds()?;
    let base_addr = base as *const u8;
    unsafe {
        let dos_header = &*(base_addr as *const IMAGE_DOS_HEADER);
        if dos_header.e_magic != 0x5A4D {
            return None;
        }

        let nt_headers =
            &*(base_addr.offset(dos_header.e_lfanew as isize) as *const IMAGE_NT_HEADERS64);
        if nt_headers.Signature != 0x4550 {
            return None;
        }

        let sections = (nt_headers as *const IMAGE_NT_HEADERS64 as *const u8)
            .add(std::mem::size_of::<IMAGE_NT_HEADERS64>())
            as *const IMAGE_SECTION_HEADER;
        for index in 0..nt_headers.FileHeader.NumberOfSections as usize {
            let header = &*sections.add(index);
            if section_name(header.Name) != section {
                continue;
            }
            let size = header.Misc.VirtualSize.max(header.SizeOfRawData) as usize;
            let start = base.checked_add(header.VirtualAddress as usize)?;
            let image_end = base.checked_add(image_size)?;
            let end = start.checked_add(size)?;
            return (size != 0 && start >= base && end <= image_end).then_some((start, size));
        }
    }
    None
}

fn scan_matches(signature: &str, limit: usize) -> Vec<usize> {
    let (bytes, mask) = parse_signature(signature);

    let Some((base, size)) = image_bounds() else {
        return Vec::new();
    };
    scan_pattern_in_range(&bytes, &mask, limit, base, size)
}

fn parse_signature(signature: &str) -> (Vec<u8>, Vec<bool>) {
    let mut bytes = Vec::new();
    let mut mask = Vec::new();

    for part in signature.split_whitespace() {
        match part {
            "?" | "??" => {
                bytes.push(0);
                mask.push(false);
            }
            _ => {
                if let Ok(byte) = u8::from_str_radix(part, 16) {
                    bytes.push(byte);
                    mask.push(true);
                }
            }
        }
    }
    (bytes, mask)
}

fn scan_pattern_in_range(
    bytes: &[u8],
    mask: &[bool],
    limit: usize,
    base: usize,
    size: usize,
) -> Vec<usize> {
    if bytes.is_empty() || bytes.len() > size {
        return Vec::new();
    }

    let end = base + size - bytes.len();
    let mut current = base;
    let mut matches = Vec::new();

    while current <= end {
        let mut found = true;
        for i in 0..bytes.len() {
            if mask[i] {
                let mem_byte = unsafe { *(current as *const u8).add(i) };
                if mem_byte != bytes[i] {
                    found = false;
                    break;
                }
            }
        }
        if found {
            matches.push(current);
            if matches.len() == limit {
                break;
            }
        }
        current += 1;
    }

    matches
}

fn section_name(raw: [u8; 8]) -> String {
    let length = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..length]).to_string()
}

fn read_module_info() -> (usize, usize) {
    let handle = unsafe { GetModuleHandleW(None).unwrap_or_default() };
    if handle.is_invalid() {
        return (0, 0);
    }

    let base_addr = handle.0 as *const u8;
    unsafe {
        let dos_header = &*(base_addr as *const IMAGE_DOS_HEADER);
        if dos_header.e_magic != 0x5A4D {
            return (0, 0);
        }

        let nt_headers =
            &*(base_addr.offset(dos_header.e_lfanew as isize) as *const IMAGE_NT_HEADERS64);
        if nt_headers.Signature != 0x4550 {
            return (0, 0);
        }

        (
            base_addr as usize,
            nt_headers.OptionalHeader.SizeOfImage as usize,
        )
    }
}

/// Resolves a relative 32-bit offset to an absolute address.
/// `instruction_addr` is the address of the instruction.
/// `offset_offset` is the byte offset from `instruction_addr` where the 32-bit relative offset begins.
/// `instruction_size` is the total size of the instruction in bytes.
pub fn resolve_relative_offset(
    instruction_addr: usize,
    offset_offset: usize,
    instruction_size: usize,
) -> usize {
    let rel_offset =
        unsafe { *(instruction_addr.wrapping_add(offset_offset) as *const i32) } as isize;
    let next_inst = instruction_addr.wrapping_add(instruction_size);
    (next_inst as isize + rel_offset) as usize
}

#[cfg(test)]
mod tests {
    use super::section_name;

    #[test]
    fn section_name_stops_at_the_first_nul() {
        assert_eq!(
            section_name([b'.', b't', b'e', b'x', b't', 0, 0, 0]),
            ".text"
        );
    }
}
