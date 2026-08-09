// SPDX-License-Identifier: GPL-3.0-or-later

use core::ffi::{c_char, c_void};
use std::{mem, ptr, sync::OnceLock};

#[path = "xuser_lifecycle.rs"]
mod lifecycle;

use super::{
    abi::{
        E_FAIL, E_INVALIDARG, E_NOINTERFACE, E_NOT_SUFFICIENT_BUFFER, E_NOTIMPL, E_POINTER, Guid,
        HResult, IID_IUNKNOWN, IID_IXUSER_ADD_WITH_UI, IID_IXUSER_BASE, IID_IXUSER_GAMERTAG,
        IID_IXUSER_MSA, IID_IXUSER_PLATFORM, IID_IXUSER_SIGN_OUT, IID_IXUSER_STORE, S_OK,
        XAsyncBlock, XAsyncOp, XAsyncProviderData, XUSER_STATE_SIGNED_IN, XUserGamertagVtable,
        XUserHandle, XUserLocalId, XUserVtable,
    },
    bridge_info, bridge_warn, session, token, xasync,
};
use lifecycle::{
    E_GAMEUSER_SIGNED_OUT, E_GAMEUSER_USER_NOT_FOUND, XTaskQueueRegistrationToken,
    XUSER_STATE_SIGNING_OUT, XUserChangeEventCallback,
};

#[repr(C)]
struct XUserGamertagInterface {
    vtable: *const XUserGamertagVtable,
}

#[repr(C)]
struct XUserObject {
    vtable: *const XUserVtable,
    gamertag: XUserGamertagInterface,
}

unsafe impl Send for XUserObject {}
unsafe impl Sync for XUserObject {}

static USER_VTABLE: OnceLock<XUserVtable> = OnceLock::new();
static GAMERTAG_VTABLE: OnceLock<XUserGamertagVtable> = OnceLock::new();
static USER_OBJECT: OnceLock<XUserObject> = OnceLock::new();
static XUSER_ADD_IDENTITY: u8 = 0;
const XUSER_ADD_NAME: &[u8] = b"XUserAddAsync\0";

fn user_object() -> Option<&'static XUserObject> {
    session()?;
    Some(USER_OBJECT.get_or_init(|| XUserObject {
        vtable: user_vtable(),
        gamertag: XUserGamertagInterface {
            vtable: gamertag_vtable(),
        },
    }))
}

pub fn provider_interface() -> Option<*mut c_void> {
    Some(user_object()? as *const XUserObject as *mut c_void)
}

fn gamertag_interface() -> Option<*mut c_void> {
    let object = user_object()?;
    Some(&object.gamertag as *const XUserGamertagInterface as *mut c_void)
}

pub fn valid_user(user: XUserHandle) -> bool {
    provider_interface().is_some_and(|provider| user == provider)
}

pub unsafe fn query_interface(iid: *const Guid, out: *mut *mut c_void) -> HResult {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    unsafe {
        out.write(ptr::null_mut());
    }

    let iid = unsafe { &*iid };

    if [
        IID_IUNKNOWN,
        IID_IXUSER_BASE,
        IID_IXUSER_ADD_WITH_UI,
        IID_IXUSER_MSA,
        IID_IXUSER_STORE,
        IID_IXUSER_PLATFORM,
        IID_IXUSER_SIGN_OUT,
    ]
    .contains(iid)
    {
        if let Some(interface) = provider_interface() {
            unsafe {
                out.write(interface);
            }
            return S_OK;
        }
    }

    if *iid == IID_IXUSER_GAMERTAG {
        if let Some(interface) = gamertag_interface() {
            unsafe {
                out.write(interface);
            }
            return S_OK;
        }
    }
    E_NOINTERFACE
}

unsafe extern "system" fn user_query_interface(
    _interface: *mut c_void,
    iid: *const Guid,
    out: *mut *mut c_void,
) -> HResult {
    unsafe { query_interface(iid, out) }
}

unsafe extern "system" fn add_ref(_interface: *mut c_void) -> u32 {
    2
}

unsafe extern "system" fn release(_interface: *mut c_void) -> u32 {
    1
}

unsafe extern "system" fn duplicate_handle(
    _interface: *mut c_void,
    user: XUserHandle,
    duplicated: *mut XUserHandle,
) -> HResult {
    if duplicated.is_null() {
        return E_POINTER;
    }
    let Some(provider) = provider_interface() else {
        return E_FAIL;
    };
    if user != provider {
        return E_INVALIDARG;
    }
    let status = lifecycle::duplicate_active_handle();
    if status < 0 {
        return status;
    }
    unsafe {
        duplicated.write(provider);
    }
    S_OK
}

unsafe extern "system" fn close_handle(_interface: *mut c_void, user: XUserHandle) {
    if !valid_user(user) {
        bridge_warn("XUserCloseHandle 收到未知用户句柄；已忽略");
        return;
    }
    match lifecycle::release_user_handle() {
        Some(remaining) => bridge_info(&format!(
            "XUserCloseHandle 已处理 | remaining_handles={remaining}"
        )),
        None => bridge_warn("XUserCloseHandle 检测到句柄引用计数下溢；已忽略重复关闭"),
    }
}

unsafe extern "system" fn compare(
    _interface: *mut c_void,
    user1: XUserHandle,
    user2: XUserHandle,
) -> i32 {
    i32::from(user1 != user2)
}

unsafe extern "system" fn get_max_users(_interface: *mut c_void, max_users: *mut u32) -> HResult {
    if max_users.is_null() {
        return E_POINTER;
    }
    unsafe {
        max_users.write(1);
    }
    S_OK
}

struct XUserAddContext {
    handle: usize,
    claimed: bool,
}

unsafe extern "system" fn xuser_add_provider(
    operation: XAsyncOp,
    provider_data: *const XAsyncProviderData,
) -> HResult {
    if provider_data.is_null() {
        return E_POINTER;
    }
    let provider_data = unsafe { &*provider_data };
    let context = provider_data.context.cast::<XUserAddContext>();
    if context.is_null() {
        return E_POINTER;
    }

    match operation {
        XAsyncOp::Begin => unsafe { xasync::schedule(provider_data.async_block, 0) },
        XAsyncOp::DoWork => {
            unsafe {
                xasync::complete(
                    provider_data.async_block,
                    S_OK,
                    mem::size_of::<XUserHandle>(),
                );
            }
            S_OK
        }
        XAsyncOp::GetResult => {
            if provider_data.buffer.is_null()
                || provider_data.buffer_size < mem::size_of::<XUserHandle>()
            {
                return E_NOT_SUFFICIENT_BUFFER;
            }
            let context = unsafe { &mut *context };
            if !context.claimed {
                let status = lifecycle::acquire_added_handle();
                if status < 0 {
                    return status;
                }
                context.claimed = true;
            }
            unsafe {
                provider_data
                    .buffer
                    .cast::<XUserHandle>()
                    .write(context.handle as XUserHandle);
            }
            S_OK
        }
        XAsyncOp::Cancel => S_OK,
        XAsyncOp::Cleanup => {
            unsafe {
                drop(Box::from_raw(context));
            }
            S_OK
        }
    }
}

unsafe extern "system" fn add_async(
    _interface: *mut c_void,
    _options: u32,
    async_block: *mut XAsyncBlock,
) -> HResult {
    if async_block.is_null() {
        return E_POINTER;
    }
    if lifecycle::state() == XUSER_STATE_SIGNING_OUT {
        return E_GAMEUSER_SIGNED_OUT;
    }
    let Some(handle) = provider_interface() else {
        return E_FAIL;
    };
    let context = Box::into_raw(Box::new(XUserAddContext {
        handle: handle as usize,
        claimed: false,
    }));
    let result = unsafe {
        xasync::begin(
            async_block,
            context.cast(),
            (&XUSER_ADD_IDENTITY as *const u8).cast(),
            XUSER_ADD_NAME.as_ptr().cast(),
            xuser_add_provider,
        )
    };
    if result < 0 {
        unsafe {
            drop(Box::from_raw(context));
        }
    }
    result
}

unsafe extern "system" fn add_result(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    user: *mut XUserHandle,
) -> HResult {
    if async_block.is_null() || user.is_null() {
        return E_POINTER;
    }
    unsafe {
        xasync::get_result(
            async_block,
            (&XUSER_ADD_IDENTITY as *const u8).cast(),
            mem::size_of::<XUserHandle>(),
            user.cast(),
            ptr::null_mut(),
        )
    }
}

unsafe extern "system" fn get_local_id(
    _interface: *mut c_void,
    user: XUserHandle,
    local_id: *mut XUserLocalId,
) -> HResult {
    if local_id.is_null() {
        return E_POINTER;
    }
    if !valid_user(user) {
        return E_INVALIDARG;
    }
    unsafe {
        local_id.write(session().unwrap().local_id);
    }
    S_OK
}

unsafe extern "system" fn find_user_by_local_id(
    _interface: *mut c_void,
    local_id: XUserLocalId,
    user: *mut XUserHandle,
) -> HResult {
    if user.is_null() {
        return E_POINTER;
    }
    if local_id != session().unwrap().local_id || !lifecycle::active_handle_exists() {
        return E_GAMEUSER_USER_NOT_FOUND;
    }
    let status = lifecycle::duplicate_active_handle();
    if status < 0 {
        return status;
    }
    unsafe {
        user.write(provider_interface().unwrap());
    }
    S_OK
}

unsafe extern "system" fn get_id(
    _interface: *mut c_void,
    user: XUserHandle,
    user_id: *mut u64,
) -> HResult {
    if user_id.is_null() {
        return E_POINTER;
    }
    if !valid_user(user) {
        return E_INVALIDARG;
    }
    unsafe {
        user_id.write(session().unwrap().xuid);
    }
    S_OK
}

unsafe extern "system" fn find_user_by_id(
    _interface: *mut c_void,
    user_id: u64,
    user: *mut XUserHandle,
) -> HResult {
    if user.is_null() {
        return E_POINTER;
    }
    if user_id != session().unwrap().xuid || !lifecycle::active_handle_exists() {
        return E_GAMEUSER_USER_NOT_FOUND;
    }
    let status = lifecycle::duplicate_active_handle();
    if status < 0 {
        return status;
    }
    unsafe {
        user.write(provider_interface().unwrap());
    }
    S_OK
}

unsafe extern "system" fn get_is_guest(
    _interface: *mut c_void,
    user: XUserHandle,
    is_guest: *mut u8,
) -> HResult {
    if is_guest.is_null() {
        return E_POINTER;
    }
    if !valid_user(user) {
        return E_INVALIDARG;
    }
    unsafe {
        is_guest.write(0);
    }
    S_OK
}

unsafe extern "system" fn get_state(
    _interface: *mut c_void,
    user: XUserHandle,
    state: *mut u32,
) -> HResult {
    if state.is_null() {
        return E_POINTER;
    }
    if !valid_user(user) {
        return E_INVALIDARG;
    }
    unsafe {
        state.write(lifecycle::state());
    }
    S_OK
}

unsafe extern "system" fn get_age_group(
    _interface: *mut c_void,
    user: XUserHandle,
    age_group: *mut u32,
) -> HResult {
    if age_group.is_null() {
        return E_POINTER;
    }
    if !valid_user(user) {
        return E_INVALIDARG;
    }
    unsafe {
        age_group.write(session().unwrap().age_group);
    }
    S_OK
}

unsafe extern "system" fn check_privilege(
    _interface: *mut c_void,
    user: XUserHandle,
    _options: u32,
    privilege: i32,
    has_privilege: *mut u8,
    deny_reason: *mut u32,
) -> HResult {
    if has_privilege.is_null() || deny_reason.is_null() {
        return E_POINTER;
    }
    if !valid_user(user) {
        return E_INVALIDARG;
    }
    if lifecycle::state() != XUSER_STATE_SIGNED_IN {
        return E_GAMEUSER_SIGNED_OUT;
    }
    let allowed = privilege >= 0 && session().unwrap().privileges.contains(&(privilege as u32));
    unsafe {
        has_privilege.write(u8::from(allowed));
        deny_reason.write(0);
    }
    S_OK
}

unsafe extern "system" fn register_for_change_event(
    _interface: *mut c_void,
    _queue: *mut c_void,
    context: *mut c_void,
    callback: Option<XUserChangeEventCallback>,
    registration_token: *mut XTaskQueueRegistrationToken,
) -> HResult {
    unsafe { lifecycle::register_for_change_event(context, callback, registration_token) }
}

unsafe extern "system" fn unregister_for_change_event(
    _interface: *mut c_void,
    registration_token: XTaskQueueRegistrationToken,
    wait: u8,
) -> u8 {
    unsafe { lifecycle::unregister_for_change_event(registration_token, wait) }
}

unsafe extern "system" fn get_sign_out_deferral(
    _interface: *mut c_void,
    deferral: *mut *mut c_void,
) -> HResult {
    unsafe { lifecycle::get_sign_out_deferral(deferral) }
}

unsafe extern "system" fn close_sign_out_deferral_handle(
    _interface: *mut c_void,
    deferral: *mut c_void,
) {
    unsafe {
        lifecycle::close_sign_out_deferral_handle(deferral);
    }
}

unsafe extern "system" fn get_gamertag(
    _interface: *mut c_void,
    user: XUserHandle,
    component: u32,
    size: usize,
    gamertag: *mut c_char,
    used: *mut usize,
) -> HResult {
    if gamertag.is_null() {
        return E_POINTER;
    }
    if !valid_user(user) {
        return E_INVALIDARG;
    }
    let value = match component {
        0 | 1 | 3 => session().unwrap().gamertag.as_str(),
        2 => "",
        _ => return E_INVALIDARG,
    };
    let required = value.len() + 1;
    if !used.is_null() {
        unsafe {
            used.write(required);
        }
    }
    if size < required {
        return E_NOT_SUFFICIENT_BUFFER;
    }
    unsafe {
        ptr::copy_nonoverlapping(value.as_ptr(), gamertag.cast::<u8>(), value.len());
        gamertag.add(value.len()).write(0);
    }
    S_OK
}

unsafe extern "system" fn stub_hresult(_interface: *mut c_void) -> HResult {
    E_NOTIMPL
}

unsafe extern "system" fn stub_boolean(_interface: *mut c_void) -> u8 {
    0
}

fn user_vtable() -> *const XUserVtable {
    USER_VTABLE.get_or_init(|| XUserVtable {
        slots: [
            user_query_interface as usize,
            add_ref as usize,
            release as usize,
            duplicate_handle as usize,
            close_handle as usize,
            compare as usize,
            get_max_users as usize,
            add_async as usize,
            add_result as usize,
            get_local_id as usize,
            find_user_by_local_id as usize,
            get_id as usize,
            find_user_by_id as usize,
            get_is_guest as usize,
            get_state as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            get_age_group as usize,
            check_privilege as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            token::get_token_and_signature_async as usize,
            token::get_token_and_signature_result_size as usize,
            token::get_token_and_signature_result as usize,
            token::get_token_and_signature_utf16_async as usize,
            token::get_token_and_signature_utf16_result_size as usize,
            token::get_token_and_signature_utf16_result as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            register_for_change_event as usize,
            unregister_for_change_event as usize,
            get_sign_out_deferral as usize,
            close_sign_out_deferral_handle as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_boolean as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_boolean as usize,
            stub_hresult as usize,
            stub_hresult as usize,
        ],
    })
}

fn gamertag_vtable() -> *const XUserGamertagVtable {
    GAMERTAG_VTABLE.get_or_init(|| XUserGamertagVtable {
        slots: [
            user_query_interface as usize,
            add_ref as usize,
            release as usize,
            get_gamertag as usize,
        ],
    })
}
