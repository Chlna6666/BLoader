// SPDX-License-Identifier: GPL-3.0-or-later

use core::ffi::{c_char, c_void};
use std::{
    mem, ptr,
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::{
    abi::{
        E_FAIL, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL, E_NOT_SUFFICIENT_BUFFER,
        E_POINTER, Guid, HResult, IID_IUNKNOWN, IID_IXUSER_ADD_WITH_UI, IID_IXUSER_BASE,
        IID_IXUSER_GAMERTAG, IID_IXUSER_MSA, IID_IXUSER_PLATFORM, IID_IXUSER_SIGN_OUT,
        IID_IXUSER_STORE, S_OK, XAsyncBlock, XUserGamertagVtable, XUserHandle,
        XUserLocalId, XUserVtable, XUSER_AGE_GROUP_UNKNOWN,
    },
    bridge_debug, bridge_info, bridge_warn, call_original_query, session, token,
};

const E_GAMEUSER_USER_NOT_FOUND: HResult = 0x8924_5104_u32 as i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XTaskQueueRegistrationToken {
    pub token: u64,
}

pub type XUserChangeEventCallback =
    unsafe extern "system" fn(*mut c_void, XUserLocalId, u32);

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
static NATIVE_BASE_INTERFACE: AtomicUsize = AtomicUsize::new(0);
static PRIMARY_NATIVE_USER: AtomicUsize = AtomicUsize::new(0);

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

/// Returns the untouched Microsoft XUser base interface through the
/// QueryApiImpl trampoline. The pointer is process-lifetime cached because the
/// system xgameruntime module remains loaded for the Minecraft process.
pub(crate) fn native_base_interface() -> Option<*mut c_void> {
    let cached = NATIVE_BASE_INTERFACE.load(Ordering::Acquire);
    if cached != 0 {
        return Some(cached as *mut c_void);
    }

    let mut out = ptr::null_mut();
    let status = unsafe { call_original_query(&super::abi::CLSID_XUSER_IMPL, &IID_IXUSER_BASE, &mut out) };
    if status < 0 || out.is_null() {
        bridge_warn(&format!(
            "无法取得微软官方 XUser backing provider | result=0x{:08X}",
            status as u32
        ));
        return None;
    }
    let value = out as usize;
    match NATIVE_BASE_INTERFACE.compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            bridge_info(&format!(
                "微软官方 XUser backing provider 已绑定 | interface=0x{value:X} | identity=custom | token_runtime=official"
            ));
            Some(out)
        }
        Err(existing) => Some(existing as *mut c_void),
    }
}

pub(crate) fn native_base_slot(index: usize) -> Option<usize> {
    if index >= 50 {
        return None;
    }
    let interface = native_base_interface()?;
    let vtable = unsafe { *(interface as *const *const usize) };
    if vtable.is_null() {
        return None;
    }
    let slot = unsafe { *vtable.add(index) };
    (slot != 0).then_some(slot)
}

fn remember_native_user(user: XUserHandle) {
    if !user.is_null() {
        PRIMARY_NATIVE_USER.store(user as usize, Ordering::Release);
    }
}

fn primary_native_user() -> Option<XUserHandle> {
    let value = PRIMARY_NATIVE_USER.load(Ordering::Acquire);
    (value != 0).then_some(value as XUserHandle)
}

pub fn valid_user(user: XUserHandle) -> bool {
    if user.is_null() {
        return false;
    }
    let Some(interface) = native_base_interface() else {
        return false;
    };
    let Some(slot) = native_base_slot(14) else {
        return false;
    };
    type Function = unsafe extern "system" fn(*mut c_void, XUserHandle, *mut u32) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    let mut state = 0u32;
    unsafe { function(interface, user, &mut state) >= 0 }
}

pub unsafe fn query_interface(iid: *const Guid, out: *mut *mut c_void) -> HResult {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    unsafe { out.write(ptr::null_mut()) };
    if native_base_interface().is_none() {
        return E_FAIL;
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
            unsafe { out.write(interface) };
            return S_OK;
        }
    }

    if *iid == IID_IXUSER_GAMERTAG {
        if let Some(interface) = gamertag_interface() {
            unsafe { out.write(interface) };
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
    let Some(interface) = native_base_interface() else { return E_FAIL; };
    let Some(slot) = native_base_slot(3) else { return E_FAIL; };
    type Function = unsafe extern "system" fn(*mut c_void, XUserHandle, *mut XUserHandle) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    let status = unsafe { function(interface, user, duplicated) };
    if status >= 0 && !unsafe { *duplicated }.is_null() {
        remember_native_user(unsafe { *duplicated });
    }
    status
}

unsafe extern "system" fn close_handle(_interface: *mut c_void, user: XUserHandle) {
    let Some(interface) = native_base_interface() else { return; };
    let Some(slot) = native_base_slot(4) else { return; };
    type Function = unsafe extern "system" fn(*mut c_void, XUserHandle);
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, user) };
}

unsafe extern "system" fn compare(
    _interface: *mut c_void,
    user1: XUserHandle,
    user2: XUserHandle,
) -> i32 {
    let Some(interface) = native_base_interface() else { return i32::from(user1 != user2); };
    let Some(slot) = native_base_slot(5) else { return i32::from(user1 != user2); };
    type Function = unsafe extern "system" fn(*mut c_void, XUserHandle, XUserHandle) -> i32;
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, user1, user2) }
}

unsafe extern "system" fn get_max_users(_interface: *mut c_void, max_users: *mut u32) -> HResult {
    let Some(interface) = native_base_interface() else { return E_FAIL; };
    let Some(slot) = native_base_slot(6) else { return E_FAIL; };
    type Function = unsafe extern "system" fn(*mut c_void, *mut u32) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, max_users) }
}

unsafe extern "system" fn add_async(
    _interface: *mut c_void,
    options: u32,
    async_block: *mut XAsyncBlock,
) -> HResult {
    let Some(interface) = native_base_interface() else { return E_FAIL; };
    let Some(slot) = native_base_slot(7) else { return E_FAIL; };
    type Function = unsafe extern "system" fn(*mut c_void, u32, *mut XAsyncBlock) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    bridge_debug("XUserAddAsync route=official-backing-user");
    unsafe { function(interface, options, async_block) }
}

unsafe extern "system" fn add_result(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    user: *mut XUserHandle,
) -> HResult {
    if user.is_null() {
        return E_POINTER;
    }
    let Some(interface) = native_base_interface() else { return E_FAIL; };
    let Some(slot) = native_base_slot(8) else { return E_FAIL; };
    type Function = unsafe extern "system" fn(*mut c_void, *mut XAsyncBlock, *mut XUserHandle) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    let status = unsafe { function(interface, async_block, user) };
    if status >= 0 && !unsafe { *user }.is_null() {
        remember_native_user(unsafe { *user });
        bridge_info("官方 backing XUser 已建立；对 Minecraft 暴露 BMCBL 自定义 XUID/Gamertag");
    }
    status
}

unsafe extern "system" fn get_local_id(
    _interface: *mut c_void,
    user: XUserHandle,
    local_id: *mut XUserLocalId,
) -> HResult {
    let Some(interface) = native_base_interface() else { return E_FAIL; };
    let Some(slot) = native_base_slot(9) else { return E_FAIL; };
    type Function = unsafe extern "system" fn(*mut c_void, XUserHandle, *mut XUserLocalId) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, user, local_id) }
}

unsafe extern "system" fn find_user_by_local_id(
    _interface: *mut c_void,
    local_id: XUserLocalId,
    user: *mut XUserHandle,
) -> HResult {
    if user.is_null() {
        return E_POINTER;
    }
    let Some(interface) = native_base_interface() else { return E_FAIL; };
    let Some(slot) = native_base_slot(10) else { return E_FAIL; };
    type Function = unsafe extern "system" fn(*mut c_void, XUserLocalId, *mut XUserHandle) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    let status = unsafe { function(interface, local_id, user) };
    if status >= 0 && !unsafe { *user }.is_null() {
        remember_native_user(unsafe { *user });
    }
    status
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
    unsafe { user_id.write(session().unwrap().xuid) };
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
    if user_id != session().unwrap().xuid {
        return E_GAMEUSER_USER_NOT_FOUND;
    }
    let Some(backing) = primary_native_user() else {
        return E_GAMEUSER_USER_NOT_FOUND;
    };
    unsafe { duplicate_handle(ptr::null_mut(), backing, user) }
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
    unsafe { is_guest.write(0) };
    S_OK
}

unsafe extern "system" fn get_state(
    _interface: *mut c_void,
    user: XUserHandle,
    state: *mut u32,
) -> HResult {
    let Some(interface) = native_base_interface() else { return E_FAIL; };
    let Some(slot) = native_base_slot(14) else { return E_FAIL; };
    type Function = unsafe extern "system" fn(*mut c_void, XUserHandle, *mut u32) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, user, state) }
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
    if session().unwrap().age_group != XUSER_AGE_GROUP_UNKNOWN {
        unsafe { age_group.write(session().unwrap().age_group) };
        return S_OK;
    }
    let Some(interface) = native_base_interface() else { return E_FAIL; };
    let Some(slot) = native_base_slot(19) else { return E_FAIL; };
    type Function = unsafe extern "system" fn(*mut c_void, XUserHandle, *mut u32) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, user, age_group) }
}

unsafe extern "system" fn check_privilege(
    _interface: *mut c_void,
    user: XUserHandle,
    options: u32,
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
    if !session().unwrap().privileges.is_empty() {
        let allowed = privilege >= 0
            && session().unwrap().privileges.contains(&(privilege as u32));
        unsafe {
            has_privilege.write(u8::from(allowed));
            deny_reason.write(0);
        }
        return S_OK;
    }
    let Some(interface) = native_base_interface() else { return E_FAIL; };
    let Some(slot) = native_base_slot(20) else { return E_FAIL; };
    type Function = unsafe extern "system" fn(
        *mut c_void,
        XUserHandle,
        u32,
        i32,
        *mut u8,
        *mut u32,
    ) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, user, options, privilege, has_privilege, deny_reason) }
}

unsafe extern "system" fn register_for_change_event(
    _interface: *mut c_void,
    queue: *mut c_void,
    context: *mut c_void,
    callback: Option<XUserChangeEventCallback>,
    registration_token: *mut XTaskQueueRegistrationToken,
) -> HResult {
    let Some(interface) = native_base_interface() else { return E_FAIL; };
    let Some(slot) = native_base_slot(33) else { return E_FAIL; };
    type Function = unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        Option<XUserChangeEventCallback>,
        *mut XTaskQueueRegistrationToken,
    ) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, queue, context, callback, registration_token) }
}

unsafe extern "system" fn unregister_for_change_event(
    _interface: *mut c_void,
    registration_token: XTaskQueueRegistrationToken,
    wait: u8,
) -> u8 {
    let Some(interface) = native_base_interface() else { return 0; };
    let Some(slot) = native_base_slot(34) else { return 0; };
    type Function = unsafe extern "system" fn(*mut c_void, XTaskQueueRegistrationToken, u8) -> u8;
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, registration_token, wait) }
}

unsafe extern "system" fn get_sign_out_deferral(
    _interface: *mut c_void,
    deferral: *mut *mut c_void,
) -> HResult {
    let Some(interface) = native_base_interface() else { return E_FAIL; };
    let Some(slot) = native_base_slot(35) else { return E_FAIL; };
    type Function = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, deferral) }
}

unsafe extern "system" fn close_sign_out_deferral_handle(
    _interface: *mut c_void,
    deferral: *mut c_void,
) {
    let Some(interface) = native_base_interface() else { return; };
    let Some(slot) = native_base_slot(36) else { return; };
    type Function = unsafe extern "system" fn(*mut c_void, *mut c_void);
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, deferral) };
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
        unsafe { used.write(required) };
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
