// SPDX-License-Identifier: GPL-3.0-or-later

use core::ffi::c_void;
use minhook::MinHook;
use std::{
    cell::Cell,
    collections::HashMap,
    mem, ptr,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use super::super::{
    abi::{E_FAIL, E_POINTER, HResult, S_OK, XUSER_STATE_SIGNED_IN, XUserLocalId},
    bridge_info, bridge_warn, session,
};

pub const XUSER_STATE_SIGNING_OUT: u32 = 1;
pub const XUSER_STATE_SIGNED_OUT: u32 = 2;
const XUSER_CHANGE_EVENT_SIGNED_IN_AGAIN: u32 = 0;
const XUSER_CHANGE_EVENT_SIGNING_OUT: u32 = 1;
const XUSER_CHANGE_EVENT_SIGNED_OUT: u32 = 2;
const PROCESS_SIGN_OUT_WAIT: Duration = Duration::from_millis(2_000);

pub const E_GAMEUSER_SIGNED_OUT: HResult = 0x8924_5101_u32 as i32;
pub const E_GAMEUSER_DEFERRAL_NOT_AVAILABLE: HResult = 0x8924_5103_u32 as i32;
pub const E_GAMEUSER_USER_NOT_FOUND: HResult = 0x8924_5104_u32 as i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XTaskQueueRegistrationToken {
    pub token: u64,
}

pub type XUserChangeEventCallback = unsafe extern "system" fn(*mut c_void, XUserLocalId, u32);

type RtlExitUserProcessFn = unsafe extern "system" fn(i32);

struct ChangeRegistration {
    token: u64,
    context: usize,
    callback: XUserChangeEventCallback,
    active: AtomicBool,
    in_flight: Mutex<usize>,
    idle: Condvar,
}

impl ChangeRegistration {
    fn begin_callback(&self) -> bool {
        if !self.active.load(Ordering::Acquire) {
            return false;
        }
        let mut in_flight = match self.in_flight.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !self.active.load(Ordering::Acquire) {
            return false;
        }
        *in_flight = in_flight.saturating_add(1);
        true
    }

    fn finish_callback(self: &Arc<Self>) {
        let idle = {
            let mut in_flight = match self.in_flight.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            *in_flight = in_flight.saturating_sub(1);
            let idle = *in_flight == 0;
            if idle {
                self.idle.notify_all();
            }
            idle
        };
        if idle && !self.active.load(Ordering::Acquire) {
            remove_registration_if_same(self.token, self);
        }
    }
}

#[repr(C)]
struct SignOutDeferral {
    marker: u64,
}

static USER_STATE: AtomicU32 = AtomicU32::new(XUSER_STATE_SIGNED_IN);
static USER_HANDLE_COUNT: AtomicUsize = AtomicUsize::new(0);
static USER_WAS_ADDED: AtomicBool = AtomicBool::new(false);
static SIGNOUT_CALLBACKS_COMPLETE: AtomicBool = AtomicBool::new(true);
static SIGNOUT_DEFERRALS: AtomicUsize = AtomicUsize::new(0);
static NEXT_REGISTRATION_TOKEN: AtomicU64 = AtomicU64::new(1);
static CHANGE_REGISTRATIONS: OnceLock<Mutex<HashMap<u64, Arc<ChangeRegistration>>>> =
    OnceLock::new();
static SIGNOUT_WAIT: OnceLock<(Mutex<()>, Condvar)> = OnceLock::new();
static PROCESS_EXIT_HOOK: OnceLock<Result<(), String>> = OnceLock::new();
static ORIGINAL_RTL_EXIT_USER_PROCESS: AtomicUsize = AtomicUsize::new(0);
static PROCESS_SHUTDOWN_STARTED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static CURRENT_CALLBACK_TOKEN: Cell<u64> = const { Cell::new(0) };
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

fn registrations() -> &'static Mutex<HashMap<u64, Arc<ChangeRegistration>>> {
    CHANGE_REGISTRATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn signout_wait() -> &'static (Mutex<()>, Condvar) {
    SIGNOUT_WAIT.get_or_init(|| (Mutex::new(()), Condvar::new()))
}

fn notify_signout_waiters() {
    signout_wait().1.notify_all();
}

fn next_registration_token() -> u64 {
    loop {
        let token = NEXT_REGISTRATION_TOKEN.fetch_add(1, Ordering::Relaxed);
        if token != 0 {
            return token;
        }
    }
}

fn remove_registration_if_same(token: u64, registration: &Arc<ChangeRegistration>) {
    let mut entries = match registrations().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if entries
        .get(&token)
        .is_some_and(|current| Arc::ptr_eq(current, registration))
    {
        entries.remove(&token);
    }
}

fn dispatch_change_event(event: u32) {
    let local_id = match session() {
        Some(session) => session.local_id,
        None => return,
    };
    let callbacks = {
        let entries = match registrations().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        entries
            .values()
            .filter(|entry| entry.active.load(Ordering::Acquire))
            .cloned()
            .collect::<Vec<_>>()
    };

    // Synchronous dispatch is intentional: shutdown callbacks must be able to
    // obtain a sign-out deferral before the title unregisters its final event
    // callback or enters RtlExitUserProcess.
    for registration in callbacks {
        if !registration.begin_callback() {
            continue;
        }
        let previous = CURRENT_CALLBACK_TOKEN.with(|current| current.replace(registration.token));
        unsafe {
            (registration.callback)(registration.context as *mut c_void, local_id, event);
        }
        CURRENT_CALLBACK_TOKEN.with(|current| current.set(previous));
        registration.finish_callback();
    }
}

pub fn state() -> u32 {
    USER_STATE.load(Ordering::Acquire)
}

pub fn active_handle_exists() -> bool {
    // XUserCloseHandle releases a title-owned handle; it does not sign the Xbox
    // user out. A user can be found again while the authenticated title session
    // remains SignedIn, even when the diagnostic handle count temporarily hits
    // zero.
    state() == XUSER_STATE_SIGNED_IN
}

pub fn acquire_added_handle() -> HResult {
    loop {
        match state() {
            XUSER_STATE_SIGNED_IN => {
                USER_WAS_ADDED.store(true, Ordering::Release);
                USER_HANDLE_COUNT.fetch_add(1, Ordering::AcqRel);
                return S_OK;
            }
            XUSER_STATE_SIGNED_OUT => {
                if PROCESS_SHUTDOWN_STARTED.load(Ordering::Acquire) {
                    return E_GAMEUSER_SIGNED_OUT;
                }
                if USER_STATE
                    .compare_exchange(
                        XUSER_STATE_SIGNED_OUT,
                        XUSER_STATE_SIGNED_IN,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    USER_WAS_ADDED.store(true, Ordering::Release);
                    USER_HANDLE_COUNT.store(1, Ordering::Release);
                    SIGNOUT_DEFERRALS.store(0, Ordering::Release);
                    SIGNOUT_CALLBACKS_COMPLETE.store(true, Ordering::Release);
                    bridge_info("XUser 生命周期已恢复为 SignedIn；正在发送 SignedInAgain 事件");
                    dispatch_change_event(XUSER_CHANGE_EVENT_SIGNED_IN_AGAIN);
                    return S_OK;
                }
            }
            XUSER_STATE_SIGNING_OUT => return E_GAMEUSER_SIGNED_OUT,
            _ => return E_FAIL,
        }
    }
}

pub fn duplicate_active_handle() -> HResult {
    if state() != XUSER_STATE_SIGNED_IN {
        return E_GAMEUSER_SIGNED_OUT;
    }
    USER_HANDLE_COUNT.fetch_add(1, Ordering::AcqRel);
    S_OK
}

pub fn release_user_handle() -> Option<usize> {
    let previous = USER_HANDLE_COUNT.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
        (count != 0).then_some(count - 1)
    });
    match previous {
        Ok(count) => {
            // Closing the final title-owned handle is not an account sign-out.
            // Sign-out is driven by the final change-event unregistration or by
            // the normal process exit hook below.
            Some(count - 1)
        }
        Err(_) => None,
    }
}

fn begin_sign_out(trigger: &str) -> bool {
    if !USER_WAS_ADDED.load(Ordering::Acquire) {
        return false;
    }
    if USER_STATE
        .compare_exchange(
            XUSER_STATE_SIGNED_IN,
            XUSER_STATE_SIGNING_OUT,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }

    SIGNOUT_CALLBACKS_COMPLETE.store(false, Ordering::Release);
    bridge_info(&format!(
        "XUser 状态进入 SigningOut | trigger={trigger} | diagnostic_handles={}",
        USER_HANDLE_COUNT.load(Ordering::Acquire)
    ));
    dispatch_change_event(XUSER_CHANGE_EVENT_SIGNING_OUT);
    SIGNOUT_CALLBACKS_COMPLETE.store(true, Ordering::Release);
    try_complete_sign_out();
    true
}

fn try_complete_sign_out() {
    if !SIGNOUT_CALLBACKS_COMPLETE.load(Ordering::Acquire)
        || SIGNOUT_DEFERRALS.load(Ordering::Acquire) != 0
    {
        return;
    }
    if USER_STATE
        .compare_exchange(
            XUSER_STATE_SIGNING_OUT,
            XUSER_STATE_SIGNED_OUT,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        bridge_info("XUser 注销回调与延迟已完成；XUser 状态进入 SignedOut");
        dispatch_change_event(XUSER_CHANGE_EVENT_SIGNED_OUT);
        notify_signout_waiters();
    }
}

fn wait_for_signed_out(timeout: Duration) -> bool {
    if state() == XUSER_STATE_SIGNED_OUT {
        return true;
    }
    let (lock, idle) = signout_wait();
    let mut guard = match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let deadline = Instant::now() + timeout;
    loop {
        if state() == XUSER_STATE_SIGNED_OUT {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let remaining = deadline.saturating_duration_since(now);
        let (next_guard, wait_result) = match idle.wait_timeout(guard, remaining) {
            Ok(result) => result,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard = next_guard;
        if wait_result.timed_out() && state() != XUSER_STATE_SIGNED_OUT {
            return false;
        }
    }
}

fn force_complete_sign_out() {
    if USER_STATE
        .compare_exchange(
            XUSER_STATE_SIGNING_OUT,
            XUSER_STATE_SIGNED_OUT,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        bridge_warn(&format!(
            "XUser 注销等待超时；进程即将退出，强制进入 SignedOut | remaining_deferrals={}",
            SIGNOUT_DEFERRALS.load(Ordering::Acquire)
        ));
        dispatch_change_event(XUSER_CHANGE_EVENT_SIGNED_OUT);
        notify_signout_waiters();
    }
}

fn begin_process_shutdown(exit_status: i32) {
    bridge_info(&format!(
        "检测到正常进程退出；正在提交 XUser 注销生命周期 | source=ntdll!RtlExitUserProcess | exit_status=0x{:08X}",
        exit_status as u32
    ));

    if !USER_WAS_ADDED.load(Ordering::Acquire) {
        USER_STATE.store(XUSER_STATE_SIGNED_OUT, Ordering::Release);
        notify_signout_waiters();
        bridge_info("进程退出时没有已添加的 XUser；无需派发注销回调");
        return;
    }

    begin_sign_out("process-exit");
    if wait_for_signed_out(PROCESS_SIGN_OUT_WAIT) {
        bridge_info("进程退出前 XUser 注销生命周期已完成");
    } else {
        force_complete_sign_out();
    }
}

unsafe extern "system" fn rtl_exit_user_process_hook(exit_status: i32) {
    if !PROCESS_SHUTDOWN_STARTED.swap(true, Ordering::AcqRel) {
        begin_process_shutdown(exit_status);
    }

    let original_address = ORIGINAL_RTL_EXIT_USER_PROCESS.load(Ordering::Acquire);
    if original_address == 0 {
        bridge_warn("RtlExitUserProcess trampoline 不可用；无法转发正常退出调用");
        return;
    }
    let original: RtlExitUserProcessFn = unsafe { mem::transmute(original_address) };
    unsafe {
        original(exit_status);
    }
}

fn install_process_exit_hook() -> Result<(), String> {
    let module_name = wide("ntdll.dll");
    let module = unsafe { GetModuleHandleW(module_name.as_ptr()) };
    if module.is_null() {
        return Err("ntdll.dll is not loaded".to_string());
    }
    let target = unsafe { GetProcAddress(module, b"RtlExitUserProcess\0".as_ptr()) };
    if target.is_null() {
        return Err("ntdll.dll does not export RtlExitUserProcess".to_string());
    }

    let trampoline = unsafe {
        MinHook::create_hook(
            target,
            rtl_exit_user_process_hook as *const () as *mut c_void,
        )
    }
    .map_err(|status| format!("MinHook create RtlExitUserProcess failed: {status:?}"))?;
    ORIGINAL_RTL_EXIT_USER_PROCESS.store(trampoline as usize, Ordering::Release);
    unsafe { MinHook::enable_all_hooks() }
        .map_err(|status| format!("MinHook enable RtlExitUserProcess failed: {status:?}"))?;

    bridge_info(&format!(
        "已安装正常退出生命周期 Hook | target=ntdll!RtlExitUserProcess | address=0x{:X} | trampoline=0x{:X}",
        target as usize, trampoline as usize
    ));
    Ok(())
}

fn ensure_process_exit_hook() -> Result<(), String> {
    PROCESS_EXIT_HOOK
        .get_or_init(install_process_exit_hook)
        .clone()
}

fn restore_signed_in_for_registration() -> bool {
    if PROCESS_SHUTDOWN_STARTED.load(Ordering::Acquire) {
        return false;
    }
    if USER_STATE
        .compare_exchange(
            XUSER_STATE_SIGNED_OUT,
            XUSER_STATE_SIGNED_IN,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        SIGNOUT_DEFERRALS.store(0, Ordering::Release);
        SIGNOUT_CALLBACKS_COMPLETE.store(true, Ordering::Release);
        bridge_info("XUser 变更订阅重新建立；生命周期恢复为 SignedIn");
        return true;
    }
    false
}

pub unsafe fn register_for_change_event(
    context: *mut c_void,
    callback: Option<XUserChangeEventCallback>,
    token: *mut XTaskQueueRegistrationToken,
) -> HResult {
    if token.is_null() || callback.is_none() {
        return E_POINTER;
    }
    if let Err(error) = ensure_process_exit_hook() {
        bridge_warn(&format!(
            "无法安装正常退出生命周期 Hook；进程退出时可能无法派发 XUser 注销事件 | reason={error}"
        ));
    }

    let restored = restore_signed_in_for_registration();
    let token_value = next_registration_token();
    let registration = Arc::new(ChangeRegistration {
        token: token_value,
        context: context as usize,
        callback: callback.unwrap(),
        active: AtomicBool::new(true),
        in_flight: Mutex::new(0),
        idle: Condvar::new(),
    });
    {
        let mut entries = match registrations().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        entries.insert(token_value, registration);
        bridge_info(&format!(
            "XUser 变更事件已注册 | registration_token={token_value} | callback_count={}",
            entries.len()
        ));
    }
    unsafe {
        token.write(XTaskQueueRegistrationToken { token: token_value });
    }
    if restored {
        dispatch_change_event(XUSER_CHANGE_EVENT_SIGNED_IN_AGAIN);
    }
    S_OK
}

pub unsafe fn unregister_for_change_event(token: XTaskQueueRegistrationToken, wait: u8) -> u8 {
    if token.token == 0 {
        return 1;
    }
    let (registration, active_count) = {
        let entries = match registrations().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        (
            entries.get(&token.token).cloned(),
            entries
                .values()
                .filter(|entry| entry.active.load(Ordering::Acquire))
                .count(),
        )
    };
    let Some(registration) = registration else {
        return 1;
    };

    // Minecraft/XSAPI tears down its long-lived XUser change subscription on
    // normal shutdown. Dispatch SigningOut while the final callback is still
    // valid; waiting until RtlExitUserProcess alone can be too late because the
    // callback context may already have been unregistered and destroyed.
    if active_count == 1
        && state() == XUSER_STATE_SIGNED_IN
        && USER_WAS_ADDED.load(Ordering::Acquire)
        && !PROCESS_SHUTDOWN_STARTED.load(Ordering::Acquire)
    {
        bridge_info("最后一个 XUser 变更订阅正在注销；在移除回调前派发 SigningOut 生命周期");
        begin_sign_out("last-change-registration-unregister");
    }

    registration.active.store(false, Ordering::Release);
    let called_from_same_callback =
        CURRENT_CALLBACK_TOKEN.with(|current| current.get() == token.token);
    let mut in_flight = match registration.in_flight.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if *in_flight == 0 {
        drop(in_flight);
        remove_registration_if_same(token.token, &registration);
        bridge_info(&format!(
            "XUser 变更事件已注销 | registration_token={} | wait={wait}",
            token.token
        ));
        return 1;
    }
    if wait == 0 || called_from_same_callback {
        return 0;
    }
    while *in_flight != 0 {
        in_flight = match registration.idle.wait(in_flight) {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
    }
    drop(in_flight);
    remove_registration_if_same(token.token, &registration);
    bridge_info(&format!(
        "XUser 变更事件已注销并等待回调完成 | registration_token={}",
        token.token
    ));
    1
}

pub unsafe fn get_sign_out_deferral(deferral: *mut *mut c_void) -> HResult {
    if deferral.is_null() {
        return E_POINTER;
    }
    unsafe {
        deferral.write(ptr::null_mut());
    }
    if state() != XUSER_STATE_SIGNING_OUT {
        return E_GAMEUSER_DEFERRAL_NOT_AVAILABLE;
    }

    SIGNOUT_DEFERRALS.fetch_add(1, Ordering::AcqRel);
    if state() != XUSER_STATE_SIGNING_OUT {
        SIGNOUT_DEFERRALS.fetch_sub(1, Ordering::AcqRel);
        return E_GAMEUSER_DEFERRAL_NOT_AVAILABLE;
    }
    let handle = Box::into_raw(Box::new(SignOutDeferral {
        marker: 0x424c_4f41_4445_5255,
    }));
    unsafe {
        deferral.write(handle.cast());
    }
    S_OK
}

pub unsafe fn close_sign_out_deferral_handle(deferral: *mut c_void) {
    if deferral.is_null() {
        return;
    }
    let deferral = unsafe { Box::from_raw(deferral.cast::<SignOutDeferral>()) };
    if deferral.marker != 0x424c_4f41_4445_5255 {
        bridge_warn("XUserCloseSignOutDeferralHandle 收到无效句柄");
        return;
    }
    drop(deferral);
    let previous = SIGNOUT_DEFERRALS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
        (count != 0).then_some(count - 1)
    });
    if matches!(previous, Ok(1)) {
        try_complete_sign_out();
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}
