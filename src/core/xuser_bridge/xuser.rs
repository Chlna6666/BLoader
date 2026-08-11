// SPDX-License-Identifier: GPL-3.0-or-later

#[path = "xuser_lifecycle.rs"]
mod lifecycle;

use core::ffi::{c_char, c_void};
use std::{
    collections::HashSet,
    mem, ptr,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::{
    abi::{
        E_FAIL, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL, E_NOT_SUFFICIENT_BUFFER, E_POINTER,
        Guid, HResult, IID_IUNKNOWN, IID_IXUSER_ADD_WITH_UI, IID_IXUSER_BASE,
        IID_IXUSER_GAMERTAG, IID_IXUSER_MSA, IID_IXUSER_PLATFORM, IID_IXUSER_SIGN_OUT,
        IID_IXUSER_STORE, S_OK, XAsyncBlock, XAsyncOp, XAsyncProviderData,
        XUserGamertagVtable, XUserHandle, XUserLocalId, XUserVtable, XUSER_AGE_GROUP_UNKNOWN,
    },
    bridge_debug, bridge_info, bridge_warn, call_original_query, session, token, xasync,
};

use lifecycle::{XTaskQueueRegistrationToken, XUserChangeEventCallback};

const E_GAMEUSER_USER_NOT_FOUND: HResult = 0x8924_5104_u32 as i32;
const XUSER_ADD_DEFAULT_USER_SILENTLY: u32 = 0x01;
const XUSER_ADD_NAME: &[u8] = b"XUserAddAsync.BMCBL\0";
static XUSER_ADD_IDENTITY: u8 = 0x42;

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

struct XUserAddContext {
    handle: usize,
}

static USER_VTABLE: OnceLock<XUserVtable> = OnceLock::new();
static GAMERTAG_VTABLE: OnceLock<XUserGamertagVtable> = OnceLock::new();
static USER_OBJECT: OnceLock<XUserObject> = OnceLock::new();
static NATIVE_BASE_INTERFACE: AtomicUsize = AtomicUsize::new(0);
static MATCHING_NATIVE_USER: AtomicUsize = AtomicUsize::new(0);
static NATIVE_ADD_ASYNCS: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();

fn user_object() -> Option<&'static XUserObject> {
    session()?;
    Some(USER_OBJECT.get_or_init(|| XUserObject {
        vtable: user_vtable(),
        gamertag: XUserGamertagInterface {
            vtable: gamertag_vtable(),
        },
    }))
}

impl XUserObject {
    fn handle(&self) -> XUserHandle {
        self as *const Self as XUserHandle
    }

    fn is_handle(&self, handle: XUserHandle) -> bool {
        !handle.is_null() && handle == self.handle()
    }
}

pub fn provider_interface() -> Option<*mut c_void> {
    Some(user_object()?.handle())
}

fn gamertag_interface() -> Option<*mut c_void> {
    let object = user_object()?;
    Some(&object.gamertag as *const XUserGamertagInterface as *mut c_void)
}

/// Returns the untouched Microsoft XUser provider only as an optional Runtime
/// capability source. BLoader's public XUser handle never aliases this pointer.
pub(crate) fn native_base_interface() -> Option<*mut c_void> {
    let cached = NATIVE_BASE_INTERFACE.load(Ordering::Acquire);
    if cached != 0 {
        return Some(cached as *mut c_void);
    }

    let mut out = ptr::null_mut();
    let status = unsafe {
        call_original_query(
            &super::abi::CLSID_XUSER_IMPL,
            &IID_IXUSER_BASE,
            &mut out,
        )
    };
    if status < 0 || out.is_null() {
        bridge_warn(&format!(
            "微软官方 XUser provider 不可用；BMCBL 自定义 XUser 仍保持可用 | result=0x{:08X}",
            status as u32
        ));
        return None;
    }
    let value = out as usize;
    match NATIVE_BASE_INTERFACE.compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            bridge_info(&format!(
                "微软官方 XUser provider 已作为可选 Runtime 能力源绑定 | interface=0x{value:X} | custom_handle=independent | native_user_required=false"
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

/// Returns the hidden Microsoft handle established by the gated same-account
/// XUserAddAsync path. Different-account and no-system-account sessions never
/// populate this slot.
pub(crate) fn native_user_for_custom_identity() -> Option<XUserHandle> {
    let value = MATCHING_NATIVE_USER.load(Ordering::Acquire);
    (value != 0).then_some(value as XUserHandle)
}

fn remember_native_add(async_block: *mut XAsyncBlock) {
    if async_block.is_null() {
        return;
    }
    NATIVE_ADD_ASYNCS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(async_block as usize);
}

fn take_native_add(async_block: *mut XAsyncBlock) -> bool {
    if async_block.is_null() {
        return false;
    }
    NATIVE_ADD_ASYNCS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&(async_block as usize))
}

fn native_user_id(user: XUserHandle) -> Result<u64, HResult> {
    if user.is_null() {
        return Err(E_INVALIDARG);
    }
    let interface = native_base_interface().ok_or(E_FAIL)?;
    let slot = native_base_slot(11).ok_or(E_FAIL)?;
    type Function = unsafe extern "system" fn(*mut c_void, XUserHandle, *mut u64) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    let mut xuid = 0u64;
    let status = unsafe { function(interface, user, &mut xuid) };
    if status < 0 || xuid == 0 {
        return Err(if status < 0 { status } else { E_FAIL });
    }
    Ok(xuid)
}

fn close_native_user(user: XUserHandle) {
    if user.is_null() {
        return;
    }
    let Some(interface) = native_base_interface() else {
        return;
    };
    let Some(slot) = native_base_slot(4) else {
        return;
    };
    type Function = unsafe extern "system" fn(*mut c_void, XUserHandle);
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, user) };
}

pub fn valid_user(user: XUserHandle) -> bool {
    user_object().is_some_and(|object| object.is_handle(user)) && lifecycle::active_handle_exists()
}

pub unsafe fn query_interface(iid: *const Guid, out: *mut *mut c_void) -> HResult {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    unsafe { out.write(ptr::null_mut()) };

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
    let Some(object) = user_object() else {
        return E_FAIL;
    };
    if !object.is_handle(user) {
        return E_INVALIDARG;
    }
    let status = lifecycle::duplicate_active_handle();
    if status < 0 {
        return status;
    }
    unsafe { duplicated.write(object.handle()) };
    S_OK
}

unsafe extern "system" fn close_handle(_interface: *mut c_void, user: XUserHandle) {
    if user_object().is_some_and(|object| object.is_handle(user)) {
        let _ = lifecycle::release_user_handle();
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
    unsafe { max_users.write(1) };
    S_OK
}

fn xuser_add_identity() -> *const c_void {
    (&XUSER_ADD_IDENTITY as *const u8).cast()
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
                )
            };
            S_OK
        }
        XAsyncOp::GetResult => {
            if provider_data.buffer.is_null()
                || provider_data.buffer_size < mem::size_of::<XUserHandle>()
            {
                return E_NOT_SUFFICIENT_BUFFER;
            }
            let handle = unsafe { (*context).handle as XUserHandle };
            unsafe { provider_data.buffer.cast::<XUserHandle>().write(handle) };
            S_OK
        }
        XAsyncOp::Cancel => S_OK,
        XAsyncOp::Cleanup => {
            unsafe { drop(Box::from_raw(context)) };
            S_OK
        }
    }
}

unsafe fn begin_synthetic_add(async_block: *mut XAsyncBlock) -> HResult {
    let Some(handle) = provider_interface() else {
        return E_FAIL;
    };
    let status = lifecycle::acquire_added_handle();
    if status < 0 {
        return status;
    }

    let context = Box::into_raw(Box::new(XUserAddContext {
        handle: handle as usize,
    }));
    let result = unsafe {
        xasync::begin(
            async_block,
            context.cast(),
            xuser_add_identity(),
            XUSER_ADD_NAME.as_ptr().cast(),
            xuser_add_provider,
        )
    };
    if result < 0 {
        unsafe { drop(Box::from_raw(context)) };
        let _ = lifecycle::release_user_handle();
        return result;
    }
    result
}

unsafe extern "system" fn add_async(
    _interface: *mut c_void,
    _options: u32,
    async_block: *mut XAsyncBlock,
) -> HResult {
    if async_block.is_null() {
        return E_POINTER;
    }

    let native_already_ready = MATCHING_NATIVE_USER.load(Ordering::Acquire) != 0;
    let same_account_hint = session().is_some_and(|runtime| runtime.native_same_account_hint());
    if same_account_hint && !native_already_ready {
        if let (Some(interface), Some(slot)) = (native_base_interface(), native_base_slot(7)) {
            type Function = unsafe extern "system" fn(*mut c_void, u32, *mut XAsyncBlock) -> HResult;
            let function: Function = unsafe { mem::transmute(slot) };
            let result = unsafe {
                function(
                    interface,
                    XUSER_ADD_DEFAULT_USER_SILENTLY,
                    async_block,
                )
            };
            if result >= 0 {
                remember_native_add(async_block);
                bridge_info(&format!(
                    "BMCBL 与 Windows 系统 XUID 提示一致；正在建立隐藏 native XUser 能力句柄 | xuid={} | native_add=AddDefaultUserSilently | public_handle=synthetic | ui=disabled",
                    session().map(|runtime| runtime.xuid).unwrap_or_default()
                ));
                return result;
            }
            bridge_warn(&format!(
                "同账号 native XUserAddAsync 无法启动；继续建立 synthetic XUser | result=0x{:08X} | token_route=pre-xsts-or-fail-closed",
                result as u32
            ));
        }
    }

    let result = unsafe { begin_synthetic_add(async_block) };
    if result >= 0 {
        bridge_debug(&format!(
            "XUserAddAsync route=bmcbl-synthetic-user | native_user_required=false | native_system_relation={} | system_login_ui=not-invoked",
            session().map_or("none", |runtime| {
                match runtime.native_system_xuid_hint {
                    Some(native) if native == runtime.xuid => "same-no-native-capability",
                    Some(_) => "different",
                    None => "none",
                }
            })
        ));
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

    if take_native_add(async_block) {
        let Some(interface) = native_base_interface() else {
            return E_FAIL;
        };
        let Some(slot) = native_base_slot(8) else {
            return E_FAIL;
        };
        type Function = unsafe extern "system" fn(
            *mut c_void,
            *mut XAsyncBlock,
            *mut XUserHandle,
        ) -> HResult;
        let function: Function = unsafe { mem::transmute(slot) };
        let mut native_user = ptr::null_mut();
        let native_result = unsafe { function(interface, async_block, &mut native_user) };
        if native_result >= 0 && !native_user.is_null() {
            match native_user_id(native_user) {
                Ok(native_xuid) if session().is_some_and(|runtime| native_xuid == runtime.xuid) => {
                    let existing = MATCHING_NATIVE_USER.compare_exchange(
                        0,
                        native_user as usize,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                    if let Err(existing) = existing {
                        if existing != native_user as usize {
                            close_native_user(native_user);
                        }
                    }
                    let status = lifecycle::acquire_added_handle();
                    if status < 0 {
                        return status;
                    }
                    let Some(handle) = provider_interface() else {
                        let _ = lifecycle::release_user_handle();
                        return E_FAIL;
                    };
                    unsafe { user.write(handle) };
                    bridge_info(&format!(
                        "隐藏 native XUser 能力句柄已验证并绑定；Minecraft 仍接收 synthetic XUser | xuid={native_xuid} | native_handle=hidden | public_handle=synthetic | token_route=same-account-native-capability"
                    ));
                    return S_OK;
                }
                Ok(native_xuid) => {
                    close_native_user(native_user);
                    bridge_warn(&format!(
                        "native XUserAddResult 返回了错误账号；拒绝绑定该能力句柄 | native_xuid={native_xuid} | custom_xuid={} | action=synthetic-only",
                        session().map(|runtime| runtime.xuid).unwrap_or_default()
                    ));
                }
                Err(status) => {
                    close_native_user(native_user);
                    bridge_warn(&format!(
                        "native XUserAddResult 身份无法验证；拒绝绑定该能力句柄 | result=0x{:08X} | action=synthetic-only",
                        status as u32
                    ));
                }
            }
        } else {
            bridge_warn(&format!(
                "同账号 native XUserAddResult 未建立可验证能力用户；返回 synthetic XUser 防止触发系统 UI | result=0x{:08X} | token_route=pre-xsts-or-fail-closed",
                native_result as u32
            ));
        }

        // The local hint can be stale. Never turn a failed silent native add into
        // an interactive add. Keep the BMCBL user alive and let the token layer
        // use pre-XSTS injection discovery/fail-closed instead.
        let status = lifecycle::acquire_added_handle();
        if status < 0 {
            return status;
        }
        let Some(handle) = provider_interface() else {
            let _ = lifecycle::release_user_handle();
            return E_FAIL;
        };
        unsafe { user.write(handle) };
        return S_OK;
    }

    let result = unsafe {
        xasync::get_result(
            async_block,
            xuser_add_identity(),
            mem::size_of::<XUserHandle>(),
            user.cast(),
            ptr::null_mut(),
        )
    };
    if result >= 0 && !unsafe { *user }.is_null() {
        bridge_info(&format!(
            "BMCBL synthetic XUser 已建立 | xuid={} | gamertag={} | native_user_required=false | system_account_optional=true",
            session().map(|value| value.xuid).unwrap_or_default(),
            session().map(|value| value.gamertag.as_str()).unwrap_or("<unknown>")
        ));
    }
    result
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
    unsafe { local_id.write(session().unwrap().local_id) };
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
    let Some(object) = user_object() else {
        return E_FAIL;
    };
    if local_id != session().unwrap().local_id || !lifecycle::active_handle_exists() {
        return E_GAMEUSER_USER_NOT_FOUND;
    }
    let status = lifecycle::duplicate_active_handle();
    if status < 0 {
        return status;
    }
    unsafe { user.write(object.handle()) };
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
    let Some(object) = user_object() else {
        return E_FAIL;
    };
    if user_id != session().unwrap().xuid || !lifecycle::active_handle_exists() {
        return E_GAMEUSER_USER_NOT_FOUND;
    }
    let status = lifecycle::duplicate_active_handle();
    if status < 0 {
        return status;
    }
    unsafe { user.write(object.handle()) };
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
    unsafe { is_guest.write(0) };
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
    let Some(object) = user_object() else {
        return E_FAIL;
    };
    if !object.is_handle(user) {
        return E_INVALIDARG;
    }
    unsafe { state.write(lifecycle::state()) };
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
    let configured = session().unwrap().age_group;
    if configured != XUSER_AGE_GROUP_UNKNOWN {
        unsafe { age_group.write(configured) };
        return S_OK;
    }

    if let Some(native) = native_user_for_custom_identity()
        && let (Some(interface), Some(slot)) = (native_base_interface(), native_base_slot(19))
    {
        type Function = unsafe extern "system" fn(*mut c_void, XUserHandle, *mut u32) -> HResult;
        let function: Function = unsafe { mem::transmute(slot) };
        return unsafe { function(interface, native, age_group) };
    }
    unsafe { age_group.write(XUSER_AGE_GROUP_UNKNOWN) };
    S_OK
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
        let allowed = privilege >= 0 && session().unwrap().privileges.contains(&(privilege as u32));
        unsafe {
            has_privilege.write(u8::from(allowed));
            deny_reason.write(0);
        }
        return S_OK;
    }

    if let Some(native) = native_user_for_custom_identity()
        && let (Some(interface), Some(slot)) = (native_base_interface(), native_base_slot(20))
    {
        type Function = unsafe extern "system" fn(
            *mut c_void,
            XUserHandle,
            u32,
            i32,
            *mut u8,
            *mut u32,
        ) -> HResult;
        let function: Function = unsafe { mem::transmute(slot) };
        return unsafe { function(interface, native, options, privilege, has_privilege, deny_reason) };
    }

    unsafe {
        has_privilege.write(0);
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
    unsafe { lifecycle::close_sign_out_deferral_handle(deferral) }
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
