// SPDX-License-Identifier: GPL-3.0-only
//! Unicode boundary for native console/CRT byte streams.
//! The code page describes the producer, not the UI language. A byte sequence
//! without a BOM cannot identify an arbitrary legacy encoding unambiguously.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteSource {
    /// WriteConsoleA / WriteFile targeting an actual Win32 console.
    Console,
    /// CRT stdout/stderr, pipes and external native byte streams.
    Native,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamFormat {
    Undecided,
    Bytes,
    Utf16Le,
    Utf16Be,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetConsoleOutputCP() -> u32;
    fn GetACP() -> u32;
    fn GetOEMCP() -> u32;
    fn MultiByteToWideChar(
        code_page: u32, flags: u32, input: *const u8, input_len: i32,
        output: *mut u16, output_len: i32,
    ) -> i32;
}

fn explicit_native_code_page() -> Option<u32> {
    static OVERRIDE: OnceLock<Option<u32>> = OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        std::env::var("BLOADER_LOG_CODE_PAGE")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| *value != 0)
    })
}

fn decode_code_page(bytes: &[u8], code_page: u32) -> Option<String> {
    if code_page == 0 || bytes.len() > i32::MAX as usize {
        return None;
    }
    unsafe {
        let needed = MultiByteToWideChar(
            code_page, 0, bytes.as_ptr(), bytes.len() as i32,
            std::ptr::null_mut(), 0,
        );
        if needed <= 0 {
            return None;
        }
        let mut wide = vec![0u16; needed as usize];
        let written = MultiByteToWideChar(
            code_page, 0, bytes.as_ptr(), bytes.len() as i32,
            wide.as_mut_ptr(), needed,
        );
        (written > 0).then(|| String::from_utf16_lossy(&wide[..written.max(0) as usize]))
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let wide: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    Some(String::from_utf16_lossy(&wide))
}

pub fn decode_bytes(bytes: &[u8], source: ByteSource) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if let Some(payload) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        if let Some(decoded) = decode_utf16(payload, true) {
            return decoded;
        }
    }
    if let Some(payload) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        if let Some(decoded) = decode_utf16(payload, false) {
            return decoded;
        }
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if let Ok(utf8) = std::str::from_utf8(bytes) {
        return utf8.to_string();
    }
    let pages = unsafe {
        match source {
            ByteSource::Console => [GetConsoleOutputCP(), GetACP(), GetOEMCP()],
            ByteSource::Native => [
                explicit_native_code_page().unwrap_or_else(|| GetACP()),
                GetConsoleOutputCP(),
                GetOEMCP(),
            ],
        }
    };
    for page in pages {
        if let Some(decoded) = decode_code_page(bytes, page) {
            return decoded;
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Stateful line buffering preserves split UTF-8, DBCS, and UTF-16 code units.
pub struct TextLineBuffer {
    pending: Vec<u8>,
    format: StreamFormat,
    source: ByteSource,
}

impl TextLineBuffer {
    pub fn new(source: ByteSource) -> Self {
        Self { pending: Vec::new(), format: StreamFormat::Undecided, source }
    }

    pub fn buffered_len(&self) -> usize {
        self.pending.len()
    }

    fn detect_format(&mut self) {
        if self.format != StreamFormat::Undecided || self.pending.len() < 4 {
            return;
        }
        if self.pending.starts_with(&[0xFF, 0xFE]) {
            self.format = StreamFormat::Utf16Le;
            self.pending.drain(..2);
        } else if self.pending.starts_with(&[0xFE, 0xFF]) {
            self.format = StreamFormat::Utf16Be;
            self.pending.drain(..2);
        } else if self.pending.starts_with(&[0xEF, 0xBB, 0xBF]) {
            self.format = StreamFormat::Bytes;
            self.pending.drain(..3);
        } else {
            // A BOM-less UTF-16 stream with mostly ASCII characters has
            // predictable NUL positions. Never reinterpret ordinary UTF-8
            // or DBCS as UTF-16 based on one accidental zero byte.
            let sample = &self.pending[..self.pending.len().min(64)];
            let pairs = sample.len() / 2;
            let even_zero = sample.iter().step_by(2).filter(|byte| **byte == 0).count();
            let odd_zero = sample.iter().skip(1).step_by(2).filter(|byte| **byte == 0).count();
            self.format = if pairs >= 4 && odd_zero * 4 >= pairs * 3 && even_zero == 0 {
                StreamFormat::Utf16Le
            } else if pairs >= 4 && even_zero * 4 >= pairs * 3 && odd_zero == 0 {
                StreamFormat::Utf16Be
            } else {
                StreamFormat::Bytes
            };
        }
    }

    fn decode_pending(&self, bytes: &[u8]) -> String {
        let text = match self.format {
            StreamFormat::Utf16Le => decode_utf16(bytes, true)
                .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
            StreamFormat::Utf16Be => decode_utf16(bytes, false)
                .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
            StreamFormat::Undecided | StreamFormat::Bytes => decode_bytes(bytes, self.source),
        };
        text.trim_end_matches(|ch| ch == '\r' || ch == '\n')
            .trim_matches('\0')
            .to_string()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(chunk);
        self.detect_format();
        if self.format == StreamFormat::Undecided {
            return Vec::new();
        }
        let mut result = Vec::new();
        loop {
            let end = match self.format {
                StreamFormat::Utf16Le | StreamFormat::Utf16Be => {
                    let little = self.format == StreamFormat::Utf16Le;
                    self.pending.chunks_exact(2).enumerate().find_map(|(index, pair)| {
                        let character = if little {
                            u16::from_le_bytes([pair[0], pair[1]])
                        } else {
                            u16::from_be_bytes([pair[0], pair[1]])
                        };
                        (character == 10).then_some((index + 1) * 2)
                    })
                }
                StreamFormat::Bytes => self.pending.iter().position(|byte| *byte == b'\n')
                    .map(|index| index + 1),
                StreamFormat::Undecided => None,
            };
            let Some(end) = end else { break };
            let line = self.decode_pending(&self.pending[..end]);
            self.pending.drain(..end);
            result.push(line);
        }
        result
    }

    pub fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        self.detect_format();
        let bytes = std::mem::take(&mut self.pending);
        let text = self.decode_pending(&bytes);
        (!text.is_empty()).then_some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_and_utf16_bom_round_trip() {
        assert_eq!(decode_bytes("中文日本語".as_bytes(), ByteSource::Native), "中文日本語");
        assert_eq!(decode_bytes(&[0xFF, 0xFE, 0x2D, 0x4E, 0x87, 0x65], ByteSource::Native), "中文");
        assert_eq!(decode_bytes(&[0xFE, 0xFF, 0x4E, 0x2D, 0x65, 0x87], ByteSource::Native), "中文");
    }

    #[test]
    fn windows_multibyte_code_pages() {
        assert_eq!(decode_code_page(&[0xD6, 0xD0, 0xCE, 0xC4], 936).as_deref(), Some("中文"));
        assert_eq!(decode_code_page(&[0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA], 932).as_deref(), Some("日本語"));
    }

    #[test]
    fn preserves_split_multibyte_sequences() {
        let mut decoder = TextLineBuffer::new(ByteSource::Native);
        assert!(decoder.push(&[0xE4, 0xB8]).is_empty());
        assert_eq!(decoder.push(&[0xAD, b'\n']), vec!["中"]);
        let mut utf16 = TextLineBuffer::new(ByteSource::Native);
        assert!(utf16.push(&[0xFF]).is_empty());
        assert!(utf16.push(&[0xFE, 0x2D, 0x4E, 0x0A]).is_empty());
        assert_eq!(utf16.push(&[0]), vec!["中"]);
    }
}
