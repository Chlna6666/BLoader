//! Bounded decoding of HTTP entities. Encoding is only removed from the
//! synthetic headers if replacement of a complete entity succeeded.
use super::response::{self, Rule};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Identity,
    Gzip,
    Deflate,
    Brotli,
}
impl Encoding {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "identity" => Ok(Self::Identity),
            "gzip" | "x-gzip" => Ok(Self::Gzip),
            "deflate" => Ok(Self::Deflate),
            "br" => Ok(Self::Brotli),
            other => Err(format!("unsupported Content-Encoding: {other}")),
        }
    }
}
pub struct Modified {
    pub body: Vec<u8>,
    /// Modified responses always use an uncompressed HTTP entity.
    pub strip_encoding: bool,
}
fn take_bounded(mut input: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    input
        .by_ref()
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut data)
        .map_err(|e| format!("decode failed: {e}"))?;
    if data.len() > limit {
        return Err("decoded response exceeds max_capture_bytes".into());
    }
    Ok(data)
}
pub fn decode(source: &[u8], kind: Encoding, limit: usize) -> Result<Vec<u8>, String> {
    match kind {
        Encoding::Identity => {
            if source.len() > limit {
                return Err("response exceeds capture bound".into());
            }
            Ok(source.to_vec())
        }
        Encoding::Gzip => {
            // An application may have already enabled WINHTTP_OPTION_DECOMPRESSION;
            // do not attempt to decode such a response twice.
            if !source.starts_with(&[0x1f, 0x8b]) {
                return Err("gzip header but no gzip entity".into());
            }
            take_bounded(flate2::read::GzDecoder::new(source), limit)
        }
        Encoding::Deflate => take_bounded(flate2::read::ZlibDecoder::new(source), limit),
        Encoding::Brotli => take_bounded(brotli::Decompressor::new(source, 4096), limit),
    }
}
pub fn rewrite(
    root: &Path,
    rule: &Rule,
    request: &[u8],
    source: &[u8],
    encoding: Encoding,
    limit: usize,
) -> Result<Option<Modified>, String> {
    let decoded = decode(source, encoding, limit)?;
    let Some(body) = response::overlay(root, rule, request, &decoded)? else {
        return Ok(None);
    };
    if body.len() > limit {
        return Err("local overlay exceeds capture limit".into());
    }
    Ok(Some(Modified {
        body,
        strip_encoding: encoding != Encoding::Identity,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn gzip_decode_roundtrip() {
        let original = br#"{"result":{"items":[]}}"#;
        let mut writer = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        writer.write_all(original).unwrap();
        let gzip = writer.finish().unwrap();
        assert_eq!(decode(&gzip, Encoding::Gzip, 4096).unwrap(), original);
        assert!(decode(&gzip, Encoding::Gzip, 2).is_err());
    }
    #[test]
    fn brotli_decode_roundtrip() {
        let original = br#"{"result":{"items":[1,2,3]}}"#;
        let mut result = Vec::new();
        {
            let mut writer = brotli::CompressorWriter::new(&mut result, 4096, 5, 22);
            writer.write_all(original).unwrap();
        }
        assert_eq!(decode(&result, Encoding::Brotli, 4096).unwrap(), original);
    }
    #[test]
    fn unknown_encoding_fails_closed() {
        assert!(Encoding::parse("zstd").is_err());
        assert!(decode(b"{}", Encoding::Identity, 2).is_ok());
    }
}
