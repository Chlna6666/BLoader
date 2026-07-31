// SPDX-License-Identifier: GPL-3.0-or-later

use base64::{Engine as _, engine::general_purpose::STANDARD};
use core::ffi::c_void;
use sha2::{Digest, Sha256};
use std::{ptr, time::{SystemTime, UNIX_EPOCH}};
use zeroize::Zeroize;

use super::abi::{E_FAIL, E_INVALIDARG, HResult};

const WINDOWS_TO_UNIX_EPOCH_SECONDS: u64 = 11_644_473_600;
const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;
const SIGNATURE_POLICY_VERSION: u32 = 1;
const P256_SIGNATURE_SIZE: usize = 64;
const SIGNATURE_HEADER_SIZE: usize = 4 + 8 + P256_SIGNATURE_SIZE;
const BCRYPT_ECDSA_PRIVATE_P256_MAGIC: u32 = 0x3253_4345;

#[link(name = "bcrypt")]
unsafe extern "system" {
    fn BCryptOpenAlgorithmProvider(
        algorithm: *mut *mut c_void,
        algorithm_id: *const u16,
        implementation: *const u16,
        flags: u32,
    ) -> i32;
    fn BCryptImportKeyPair(
        algorithm: *mut c_void,
        import_key: *mut c_void,
        blob_type: *const u16,
        key: *mut *mut c_void,
        input: *mut u8,
        input_size: u32,
        flags: u32,
    ) -> i32;
    fn BCryptSignHash(
        key: *mut c_void,
        padding_info: *mut c_void,
        input: *mut u8,
        input_size: u32,
        output: *mut u8,
        output_size: u32,
        result_size: *mut u32,
        flags: u32,
    ) -> i32;
    fn BCryptDestroyKey(key: *mut c_void) -> i32;
    fn BCryptCloseAlgorithmProvider(algorithm: *mut c_void, flags: u32) -> i32;
}

pub struct SigningKey {
    algorithm: *mut c_void,
    key: *mut c_void,
}

unsafe impl Send for SigningKey {}
unsafe impl Sync for SigningKey {}

impl Drop for SigningKey {
    fn drop(&mut self) {
        unsafe {
            if !self.key.is_null() {
                let _ = BCryptDestroyKey(self.key);
                self.key = ptr::null_mut();
            }
            if !self.algorithm.is_null() {
                let _ = BCryptCloseAlgorithmProvider(self.algorithm, 0);
                self.algorithm = ptr::null_mut();
            }
        }
    }
}

impl SigningKey {
    pub fn import_private_blob(mut blob: Vec<u8>) -> Result<Self, String> {
        let valid = blob.len() == 104
            && u32::from_le_bytes(blob[0..4].try_into().unwrap())
                == BCRYPT_ECDSA_PRIVATE_P256_MAGIC
            && u32::from_le_bytes(blob[4..8].try_into().unwrap()) == 32;
        if !valid {
            blob.zeroize();
            return Err("invalid P-256 BCRYPT private key blob".to_string());
        }

        let algorithm_name = wide("ECDSA_P256");
        let blob_type = wide("ECCPRIVATEBLOB");
        let mut algorithm = ptr::null_mut();
        let status = unsafe {
            BCryptOpenAlgorithmProvider(
                &mut algorithm,
                algorithm_name.as_ptr(),
                ptr::null(),
                0,
            )
        };
        if status != 0 {
            blob.zeroize();
            return Err(format!(
                "BCryptOpenAlgorithmProvider failed: 0x{status:08X}"
            ));
        }

        let mut key = ptr::null_mut();
        let status = unsafe {
            BCryptImportKeyPair(
                algorithm,
                ptr::null_mut(),
                blob_type.as_ptr(),
                &mut key,
                blob.as_mut_ptr(),
                blob.len() as u32,
                0,
            )
        };
        blob.zeroize();
        if status != 0 || key.is_null() {
            unsafe {
                let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
            }
            return Err(format!("BCryptImportKeyPair failed: 0x{status:08X}"));
        }

        Ok(Self { algorithm, key })
    }

    fn sign_digest(&self, digest: &mut [u8; 32]) -> Result<[u8; 64], HResult> {
        let mut signature = [0u8; P256_SIGNATURE_SIZE];
        let mut written = 0u32;
        let status = unsafe {
            BCryptSignHash(
                self.key,
                ptr::null_mut(),
                digest.as_mut_ptr(),
                digest.len() as u32,
                signature.as_mut_ptr(),
                signature.len() as u32,
                &mut written,
                0,
            )
        };
        if status != 0 || written as usize != signature.len() {
            signature.zeroize();
            return Err(if status != 0 { status } else { E_FAIL });
        }
        Ok(signature)
    }

    pub fn sign_request(
        &self,
        method: &str,
        request_target: &str,
        authorization: &str,
        policy_header_values: &[&str],
        body: &[u8],
    ) -> Result<String, HResult> {
        if method.is_empty()
            || request_target.is_empty()
            || !method.is_ascii()
            || !request_target.is_ascii()
        {
            return Err(E_INVALIDARG);
        }

        let timestamp = current_filetime();
        let mut uppercase_method = method
            .bytes()
            .map(|byte| byte.to_ascii_uppercase())
            .collect::<Vec<_>>();
        let mut hasher = Sha256::new();
        hasher.update(SIGNATURE_POLICY_VERSION.to_be_bytes());
        hasher.update([0]);
        hasher.update(timestamp.to_be_bytes());
        hasher.update([0]);
        hasher.update(&uppercase_method);
        hasher.update([0]);
        hasher.update(request_target.as_bytes());
        hasher.update([0]);
        hasher.update(authorization.as_bytes());
        hasher.update([0]);
        for value in policy_header_values {
            hasher.update(value.as_bytes());
            hasher.update([0]);
        }
        hasher.update(body);
        hasher.update([0]);
        uppercase_method.zeroize();

        let mut digest: [u8; 32] = hasher.finalize().into();
        let mut signature = self.sign_digest(&mut digest)?;
        digest.zeroize();

        let mut header = [0u8; SIGNATURE_HEADER_SIZE];
        header[..4].copy_from_slice(&SIGNATURE_POLICY_VERSION.to_be_bytes());
        header[4..12].copy_from_slice(&timestamp.to_be_bytes());
        header[12..].copy_from_slice(&signature);
        signature.zeroize();

        let encoded = STANDARD.encode(header);
        header.zeroize();
        Ok(encoded)
    }
}

fn current_filetime() -> u64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    WINDOWS_TO_UNIX_EPOCH_SECONDS
        .saturating_add(duration.as_secs())
        .saturating_mul(FILETIME_TICKS_PER_SECOND)
        .saturating_add(u64::from(duration.subsec_nanos()) / 100)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_header_shape_is_stable() {
        assert_eq!(SIGNATURE_HEADER_SIZE, 76);
        assert_eq!(STANDARD.encode([0u8; SIGNATURE_HEADER_SIZE]).len(), 104);
    }

    #[test]
    fn filetime_uses_windows_epoch() {
        assert!(current_filetime() > 116_444_736_000_000_000);
    }
}
