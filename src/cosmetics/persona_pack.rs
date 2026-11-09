//! Persona binary bundle builder, modeled on the openly documented MerelyCapes
//! layout (MIT licensed). Distinct from authentic Mojang Marketplace signing.
//! Native ZIPs are never decrypted or modified unless a bare pack requires
//! an outer ppack0.zip bundle.
use aes::Aes256;
use aes::cipher::{Block, BlockEncrypt, KeyInit};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Write};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const CONTENT_KEY: &[u8; 32] = b"s5s5ejuDru4uchuF2drUFuthaspAbepE";
const KEY_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890";

fn hash_base64(data: &[u8]) -> String {
    let d = Sha256::digest(data);
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for block in d.chunks(3) {
        let a = block[0] as usize;
        let b = block.get(1).copied().unwrap_or(0) as usize;
        let c = block.get(2).copied().unwrap_or(0) as usize;
        out.push(alphabet[a >> 2] as char);
        out.push(alphabet[((a & 3) << 4) | (b >> 4)] as char);
        out.push(if block.len() > 1 {
            alphabet[((b & 15) << 2) | (c >> 6)] as char
        } else {
            '='
        });
        out.push(if block.len() > 2 {
            alphabet[c & 63] as char
        } else {
            '='
        });
    }
    out
}
// Stable 32-byte ASCII keys keep each locally generated ZIP byte-for-byte
// reproducible without storing encryption secrets or rewriting user ZIPs.
fn file_key(pack_uuid: &str, path: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"Cosmetics/persona-file-key/v1/");
    hash.update(pack_uuid.as_bytes());
    hash.update([0]);
    hash.update(path.as_bytes());
    let digest = hash.finalize();
    let mut key = [0u8; 32];
    for (index, out) in key.iter_mut().enumerate() {
        *out = KEY_CHARS[digest[index] as usize % KEY_CHARS.len()];
    }
    key
}
// Compatible with System.Security.Cryptography.Aes.Create() used by
// MerelyCapes: AES-256, CFB-8, 128-bit block, IV=first 16 key bytes, zero
// padding to a multiple of 16 before encryption.
fn encrypt_cfb8(key: &[u8; 32], plain: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256::new_from_slice(key).map_err(|e| format!("invalid AES key: {e}"))?;
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&key[..16]);
    let mut out = plain.to_vec();
    if out.len() % 16 != 0 {
        out.resize(out.len() + (16 - out.len() % 16), 0);
    }
    for byte in &mut out {
        let mut mask = Block::<Aes256>::default();
        mask.copy_from_slice(&iv);
        cipher.encrypt_block(&mut mask);
        let encrypted = *byte ^ mask[0];
        iv.copy_within(1..16, 0);
        iv[15] = encrypted;
        *byte = encrypted;
    }
    Ok(out)
}
fn encrypted_index(pack_uuid: &str, contents: &Value) -> Result<Vec<u8>, String> {
    let plain = serde_json::to_vec(contents).map_err(|e| e.to_string())?;
    let encrypted = encrypt_cfb8(CONTENT_KEY, &plain)?;
    let id = pack_uuid.as_bytes();
    if id.len() > 239 {
        return Err("pack uuid too long for encrypted Persona index".into());
    }
    let mut bytes = Vec::with_capacity(256 + encrypted.len());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&2614082044u32.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.push(id.len() as u8);
    bytes.extend_from_slice(id);
    bytes.resize(256, 0);
    bytes.extend_from_slice(&encrypted);
    Ok(bytes)
}
fn zip_entries(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (path, data) in entries {
        writer
            .start_file(path, options)
            .map_err(|e| e.to_string())?;
        writer.write_all(data).map_err(|e| e.to_string())?;
    }
    Ok(writer.finish().map_err(|e| e.to_string())?.into_inner())
}
/// Persona catalog Contents with Type=personabinary points to a ZIP containing
/// ppack0.zip, not straight at the nested manifest.json pack.
pub fn bundle_pack(inner: &[u8]) -> Result<Vec<u8>, String> {
    zip_entries(&[("ppack0.zip".to_owned(), inner.to_vec())])
}
pub fn generate(
    name: &str,
    pack_uuid: &str,
    module_uuid: &str,
    png: &[u8],
    version: [u32; 3],
) -> Result<Vec<u8>, String> {
    let piece = format!("{module_uuid}_cape");
    let texture = format!("{piece}.png");
    let meta_path = format!("{piece}.meta.json");
    let manifest = json!({
        "format_version":1,
        "header":{"name":name,"description":"Cosmetics local Persona cape",
                  "uuid":pack_uuid,"version":version},
        "modules":[{"type":"persona_piece","uuid":module_uuid,"version":version}]
    });
    let manifest_bytes = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    let meta = json!({
        "piece_id":pack_uuid,"piece_name":piece,"piece_type":"persona_capes",
        "zone":["body_back_upper","body_back_lower"],
        "texture_sources":[{"texture":texture}]
    });
    let signature = json!([{"hash":hash_base64(&manifest_bytes),"path":"manifest.json"}]);
    let text = format!(
        "persona.{piece}.title={}\n",
        name.replace('\r', " ").replace('\n', " ")
    );
    let files = [
        ("manifest.json".to_owned(), manifest_bytes),
        (texture, png.to_vec()),
        (
            meta_path,
            serde_json::to_vec(&meta).map_err(|e| e.to_string())?,
        ),
        (
            "signatures.json".to_owned(),
            serde_json::to_vec(&signature).map_err(|e| e.to_string())?,
        ),
        ("texts/languages.json".to_owned(), br#"["en_US"]"#.to_vec()),
        ("texts/en_US.lang".to_owned(), text.into_bytes()),
    ];
    let mut inner = Vec::with_capacity(files.len() + 1);
    let mut contents = Vec::with_capacity(files.len() + 1);
    for (path, data) in files {
        if matches!(
            path.as_str(),
            "manifest.json" | "texts/languages.json" | "texts/en_US.lang"
        ) {
            contents.push(json!({"path":path}));
            inner.push((path, data));
        } else {
            let key = file_key(pack_uuid, &path);
            let ciphertext = encrypt_cfb8(&key, &data)?;
            contents.push(json!({"path":path,"key":String::from_utf8_lossy(&key)}));
            inner.push((path, ciphertext));
        }
    }
    contents.push(json!({"path":"contents.json"}));
    let index = json!({"version":1,"content":contents});
    inner.push(("contents.json".into(), encrypted_index(pack_uuid, &index)?));
    bundle_pack(&zip_entries(&inner)?)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use zip::ZipArchive;
    fn decrypt_cfb8(key: &[u8; 32], ciphertext: &[u8]) -> Vec<u8> {
        let aes = Aes256::new_from_slice(key).unwrap();
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&key[..16]);
        let mut out = Vec::with_capacity(ciphertext.len());
        for &cipher_byte in ciphertext {
            let mut mask = Block::<Aes256>::default();
            mask.copy_from_slice(&iv);
            aes.encrypt_block(&mut mask);
            out.push(cipher_byte ^ mask[0]);
            iv.copy_within(1..16, 0);
            iv[15] = cipher_byte;
        }
        out
    }
    #[test]
    fn aes_256_cfb8_matches_openssl_reference() {
        let key = b"abcdefghijklmnopqrstuvwxyzABCDEF";
        let ciphertext = encrypt_cfb8(key, b"hello world").unwrap();
        assert_eq!(
            &ciphertext[..11],
            &[
                0x92, 0xa4, 0x8d, 0x90, 0x4c, 0x6f, 0xd9, 0xfc, 0x96, 0x10, 0xfd
            ]
        );
        assert_eq!(&decrypt_cfb8(key, &ciphertext)[..11], b"hello world");
    }
    #[test]
    fn generated_binary_has_cosmos_bundle_and_usable_cape_metadata() {
        let id = "11111111-2222-4333-8444-555555555555";
        let module = "66666666-2222-4333-8444-555555555555";
        let png = b"PNG image placeholder";
        let bytes = generate("Test Cape", id, module, png, [1, 0, 0]).unwrap();
        let mut outer = ZipArchive::new(Cursor::new(&bytes)).unwrap();
        assert_eq!(outer.len(), 1);
        let mut nested = Vec::new();
        outer
            .by_name("ppack0.zip")
            .unwrap()
            .read_to_end(&mut nested)
            .unwrap();
        let mut pack = ZipArchive::new(Cursor::new(nested)).unwrap();
        let manifest: Value =
            serde_json::from_reader(pack.by_name("manifest.json").unwrap()).unwrap();
        assert_eq!(manifest["header"]["uuid"], id);
        let mut content_bytes = Vec::new();
        pack.by_name("contents.json")
            .unwrap()
            .read_to_end(&mut content_bytes)
            .unwrap();
        assert_eq!(&content_bytes[4..8], &2614082044u32.to_le_bytes());
        assert_eq!(content_bytes[16], 36);
        assert_eq!(&content_bytes[17..53], id.as_bytes());
        let decrypted = decrypt_cfb8(CONTENT_KEY, &content_bytes[256..]);
        let contents: Value = serde_json::from_slice(
            &decrypted[..decrypted.iter().rposition(|&b| b != 0).unwrap() + 1],
        )
        .unwrap();
        let list = contents["content"].as_array().unwrap();
        for entry in list {
            let path = entry["path"].as_str().unwrap();
            if path == "contents.json" {
                continue;
            }
            let mut data = Vec::new();
            pack.by_name(path).unwrap().read_to_end(&mut data).unwrap();
            if let Some(key) = entry["key"].as_str() {
                let key: &[u8; 32] = key.as_bytes().try_into().unwrap();
                data = decrypt_cfb8(key, &data);
            }
            if path.ends_with(".meta.json") {
                let meta: Value = serde_json::from_slice(
                    &data[..data.iter().rposition(|&b| b != 0).unwrap() + 1],
                )
                .unwrap();
                assert_eq!(meta["piece_id"], id);
                assert_eq!(meta["piece_type"], "persona_capes");
                assert_eq!(meta["zone"], json!(["body_back_upper", "body_back_lower"]));
                assert_eq!(
                    meta["texture_sources"][0]["texture"],
                    format!("{module}_cape.png")
                );
            }
            if path.ends_with("_cape.png") {
                assert_eq!(&data[..png.len()], png);
            }
        }
        assert_eq!(
            bytes,
            generate("Test Cape", id, module, png, [1, 0, 0]).unwrap()
        );
    }
}
