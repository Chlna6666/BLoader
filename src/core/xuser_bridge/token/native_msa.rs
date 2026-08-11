// SPDX-License-Identifier: GPL-3.0-only

use core::ffi::{c_char, c_void};
use minhook::MinHook;
use std::{
    collections::{HashSet},
    ffi::CStr,
    mem,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
use zeroize::Zeroize;

use super::super::{
    abi::{
        E_FAIL, E_INVALIDARG, E_NOT_SUFFICIENT_BUFFER, E_POINTER, HResult, S_OK, XAsyncBlock,
        XAsyncOp, XAsyncProviderData, XUserHandle,
    },
    bridge_debug, bridge_info, bridge_warn, session, xasync, xuser,
};

const MSA_NAME: &[u8] = b"XUserGetMsaTokenSilentlyAsync.BMCBL\0";
static MSA_IDENTITY: u8 = 0x4d;

static INIT: OnceLock<Result<(), String>> = OnceLock::new();
static ORIGINAL_ASYNC: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_RESULT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_RESULT_SIZE: AtomicUsize = AtomicUsize::new(0);
static CUSTOM_ASYNC: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
static OVERRIDE_GENERATION: AtomicU64 = AtomicU64::new(0);
static LOGGED_SCOPES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

type MsaAsyncFn = unsafe extern "system" fn(
    *mut c_void,
    XUserHandle,
    u32,
    *const c_char,
    *mut XAsyncBlock,
) -> HResult;
type MsaResultFn = unsafe extern "system" fn(
    *mut c_void,
    *mut XAsyncBlock,
    usize,
    *mut c_char,
    *mut usize,
) -> HResult;
type MsaResultSizeFn =
    unsafe extern "system" fn(*mut c_void, *mut XAsyncBlock, *mut usize) -> HResult;

struct MsaContext {
    token: Vec<u8>,
}

impl Drop for MsaContext {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

pub(super) fn initialize() -> Result<(), String> {
    INIT.get_or_init(|| unsafe { install() }).clone()
}

pub(super) fn override_generation() -> u64 {
    OVERRIDE_GENERATION.load(Ordering::Acquire)
}

unsafe fn install() -> Result<(), String> {
    let async_target = xuser::native_base_slot(39)
        .ok_or_else(|| "native XUser MSA async slot 39 unavailable".to_string())?;
    let result_target = xuser::native_base_slot(40)
        .ok_or_else(|| "native XUser MSA result slot 40 unavailable".to_string())?;
    let size_target = xuser::native_base_slot(41)
        .ok_or_else(|| "native XUser MSA result-size slot 41 unavailable".to_string())?;

    if async_target == result_target || async_target == size_target || result_target == size_target {
        return Err("native XUser MSA slots unexpectedly alias the same implementation".to_string());
    }

    let original = unsafe {
        MinHook::create_hook(
            async_target as *mut c_void,
            msa_async_hook as *const () as *mut c_void,
        )
    }
    .map_err(|status| format!("hook native MSA async failed: {status:?}"))?;
    ORIGINAL_ASYNC.store(original as usize, Ordering::Release);

    let original = unsafe {
        MinHook::create_hook(
            result_target as *mut c_void,
            msa_result_hook as *const () as *mut c_void,
        )
    }
    .map_err(|status| format!("hook native MSA result failed: {status:?}"))?;
    ORIGINAL_RESULT.store(original as usize, Ordering::Release);

    let original = unsafe {
        MinHook::create_hook(
            size_target as *mut c_void,
            msa_result_size_hook as *const () as *mut c_void,
        )
    }
    .map_err(|status| format!("hook native MSA result-size failed: {status:?}"))?;
    ORIGINAL_RESULT_SIZE.store(original as usize, Ordering::Release);

    unsafe { MinHook::enable_all_hooks() }
        .map_err(|status| format!("enable native MSA credential hooks failed: {status:?}"))?;

    bridge_info(&format!(
        "Microsoft Runtime 用户凭据覆盖桥已安装 | slots=39/40/41 | msa_async=0x{async_target:X} | msa_result=0x{result_target:X} | msa_result_size=0x{size_target:X} | mode=cross-account-only | refresh_token_present=false | secrets_logged=false"
    ));
    Ok(())
}

unsafe extern "system" fn msa_async_hook(
    interface: *mut c_void,
    user: XUserHandle,
    options: u32,
    scope: *const c_char,
    async_block: *mut XAsyncBlock,
) -> HResult {
    let original: MsaAsyncFn = unsafe { original_fn(&ORIGINAL_ASYNC) };
    if user.is_null() || async_block.is_null() {
        return unsafe { original(interface, user, options, scope, async_block) };
    }

    let Some(runtime) = session() else {
        return unsafe { original(interface, user, options, scope, async_block) };
    };
    let native_xuid = match native_user_id(user) {
        Ok(value) => value,
        Err(_) => return unsafe { original(interface, user, options, scope, async_block) },
    };
    if native_xuid == runtime.xuid {
        return unsafe { original(interface, user, options, scope, async_block) };
    }

    let Some(access_token) = runtime.custom_msa_access_token() else {
        bridge_warn(&format!(
            "跨账号 MSA 用户凭据覆盖失败 | native_xuid={native_xuid} | custom_xuid={} | reason=custom-msa-access-token-expired-or-unavailable | action=fail-closed",
            runtime.xuid,
        ));
        return E_FAIL;
    };

    let scope_text = safe_scope(scope);
    let scope_key = if scope_text.is_empty() {
        "<empty>".to_string()
    } else {
        scope_text.clone()
    };
    let inserted = LOGGED_SCOPES
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(scope_key.clone());
    if inserted {
        bridge_info(&format!(
            "已命中 Microsoft Runtime MSA 用户凭据入口 | native_xuid={native_xuid} | custom_xuid={} | scope={} | route=bmcbl-msa-access-token | token_body=redacted | refresh_token_present=false",
            runtime.xuid,
            scope_key,
        ));
    }

    let mut token = access_token.as_bytes().to_vec();
    token.push(0);
    let context = Box::into_raw(Box::new(MsaContext { token }));
    let status = unsafe {
        xasync::begin(
            async_block,
            context.cast(),
            msa_identity(),
            MSA_NAME.as_ptr().cast(),
            msa_provider,
        )
    };
    if status < 0 {
        unsafe { drop(Box::from_raw(context)) };
        return status;
    }

    CUSTOM_ASYNC
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(async_block as usize);
    let generation = OVERRIDE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    bridge_debug(&format!(
        "跨账号 MSA access token 已提交给官方认证调用方 | generation={generation} | native_xuid={native_xuid} | custom_xuid={} | token_body=redacted",
        runtime.xuid,
    ));
    S_OK
}

unsafe extern "system" fn msa_result_size_hook(
    interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    token_size: *mut usize,
) -> HResult {
    if is_custom_async(async_block) {
        return unsafe { xasync::get_result_size(async_block, token_size) };
    }
    let original: MsaResultSizeFn = unsafe { original_fn(&ORIGINAL_RESULT_SIZE) };
    unsafe { original(interface, async_block, token_size) }
}

unsafe extern "system" fn msa_result_hook(
    interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    result_token_size: usize,
    result_token: *mut c_char,
    result_token_used: *mut usize,
) -> HResult {
    if is_custom_async(async_block) {
        if async_block.is_null() || (result_token_size != 0 && result_token.is_null()) {
            return E_POINTER;
        }
        let result = unsafe {
            xasync::get_result(
                async_block,
                msa_identity(),
                result_token_size,
                result_token.cast(),
                result_token_used,
            )
        };
        if result != E_NOT_SUFFICIENT_BUFFER {
            remove_custom_async(async_block);
        }
        return result;
    }
    let original: MsaResultFn = unsafe { original_fn(&ORIGINAL_RESULT) };
    unsafe {
        original(
            interface,
            async_block,
            result_token_size,
            result_token,
            result_token_used,
        )
    }
}

unsafe extern "system" fn msa_provider(
    operation: XAsyncOp,
    provider_data: *const XAsyncProviderData,
) -> HResult {
    if provider_data.is_null() {
        return E_POINTER;
    }
    let provider_data = unsafe { &*provider_data };
    let context = provider_data.context.cast::<MsaContext>();
    if context.is_null() {
        return E_POINTER;
    }

    match operation {
        XAsyncOp::Begin => unsafe { xasync::schedule(provider_data.async_block, 0) },
        XAsyncOp::DoWork => {
            let size = unsafe { (*context).token.len() };
            unsafe { xasync::complete(provider_data.async_block, S_OK, size) };
            S_OK
        }
        XAsyncOp::GetResult => {
            let context = unsafe { &*context };
            if provider_data.buffer.is_null() || provider_data.buffer_size < context.token.len() {
                return E_NOT_SUFFICIENT_BUFFER;
            }
            unsafe {
                core::ptr::copy_nonoverlapping(
                    context.token.as_ptr(),
                    provider_data.buffer.cast::<u8>(),
                    context.token.len(),
                );
            }
            S_OK
        }
        XAsyncOp::Cancel => S_OK,
        XAsyncOp::Cleanup => {
            unsafe { drop(Box::from_raw(context)) };
            S_OK
        }
    }
}

fn native_user_id(user: XUserHandle) -> Result<u64, HResult> {
    if user.is_null() {
        return Err(E_INVALIDARG);
    }
    let interface = xuser::native_base_interface().ok_or(E_FAIL)?;
    let slot = xuser::native_base_slot(11).ok_or(E_FAIL)?;
    type Function = unsafe extern "system" fn(*mut c_void, XUserHandle, *mut u64) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    let mut xuid = 0u64;
    let status = unsafe { function(interface, user, &mut xuid) };
    if status < 0 || xuid == 0 {
        return Err(if status < 0 { status } else { E_FAIL });
    }
    Ok(xuid)
}

fn safe_scope(scope: *const c_char) -> String {
    if scope.is_null() {
        return "<null>".to_string();
    }
    let value = unsafe { CStr::from_ptr(scope) }.to_string_lossy();
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(512)
        .collect()
}

fn is_custom_async(async_block: *mut XAsyncBlock) -> bool {
    !async_block.is_null()
        && CUSTOM_ASYNC
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&(async_block as usize))
}

fn remove_custom_async(async_block: *mut XAsyncBlock) {
    if async_block.is_null() {
        return;
    }
    CUSTOM_ASYNC
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&(async_block as usize));
}

fn msa_identity() -> *const c_void {
    (&MSA_IDENTITY as *const u8).cast()
}

unsafe fn original_fn<T>(storage: &AtomicUsize) -> T
where
    T: Copy,
{
    let address = storage.load(Ordering::Acquire);
    debug_assert_ne!(address, 0);
    unsafe { mem::transmute_copy(&address) }
}
