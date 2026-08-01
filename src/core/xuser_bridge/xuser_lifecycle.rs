// SPDX-License-Identifier: GPL-3.0-or-later

use core::ffi::c_void;
use std::{
    cell::Cell,
    collections::HashMap,
    ptr,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
};

use super::super::{
    abi::{E_FAIL, E_POINTER, HResult, S_OK, XUserLocalId, XUSER_STATE_SIGNED_IN},
    bridge_info, bridge_warn, session,
};

pub const XUSER_STATE_SIGNING_OUT: u32 = 1;
pub const XUSER_STATE_SIGNED_OUT: u32 = 2;
const XUSER_CHANGE_EVENT_SIGNED_IN_AGAIN: u32 = 0;
const XUSER_CHANGE_EVENT_SIGNING_OUT: u32 = 1;
const XUSER_CHANGE_EVENT_SIGNED_OUT: u32 = 2;

pub const E_GAMEUSER_SIGNED_OUT: HResult = 0x8924_5101_u32 as i32;
pub const E_GAMEUSER_DEFERRAL_NOT_AVAILABLE: HResult = 0x8924_5103_u32 as i32;
pub const E_GAMEUSER_USER_NOT_FOUND: HResult = 0x8924_5104_u32 as i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XTaskQueueRegistrationToken {
    pub token: u64,
}

pub type XUserChangeEventCallback =
    unsafe extern "system" fn(*mut c_void, XUserLocalId, u32);

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
static SIGNOUT_CALLBACKS_COMPLETE: AtomicBool = AtomicBool::new(true);
static SIGNOUT_DEFERRALS: AtomicUsize = AtomicUsize::new(0);
static NEXT_REGISTRATION_TOKEN: AtomicU64 = AtomicU64::new(1);
static CHANGE_REGISTRATIONS: OnceLock<Mutex<HashMap<u64, Arc<ChangeRegistration>>>> =
    OnceLock::new();

thread_local! {
    static CURRENT_CALLBACK_TOKEN: Cell<u64> = const { Cell::new(0) };
}

fn registrations() -> &'static Mutex<HashMap<u64, Arc<ChangeRegistration>>> {
    CHANGE_REGISTRATIONS.get_or_init(|| Mutex::new(HashMap::new()))
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

    // Dispatch synchronously so shutdown callbacks run before the title releases
    // its final XUser handle. This also permits a callback to obtain a deferral.
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
    USER_HANDLE_COUNT.load(Ordering::Acquire) != 0 && state() != XUSER_STATE_SIGNED_OUT
}

pub fn acquire_added_handle() -> HResult {
    loop {
        match state() {
            XUSER_STATE_SIGNED_IN => {
                USER_HANDLE_COUNT.fetch_add(1, Ordering::AcqRel);
                return S_OK;
            }
            XUSER_STATE_SIGNED_OUT => {
                if USER_STATE
                    .compare_exchange(
                        XUSER_STATE_SIGNED_OUT,
                        XUSER_STATE_SIGNED_IN,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    USER_HANDLE_COUNT.store(1, Ordering::Release);
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
    if state() != XUSER_STATE_SIGNED_IN || !active_handle_exists() {
        return E_GAMEUSER_SIGNED_OUT;
    }
    USER_HANDLE_COUNT.fetch_add(1, Ordering::AcqRel);
    S_OK
}

pub fn release_user_handle() -> Option<usize> {
    let previous = USER_HANDLE_COUNT.fetch_update(
        Ordering::AcqRel,
        Ordering::Acquire,
        |count| (count != 0).then_some(count - 1),
    );
    match previous {
        Ok(count) => {
            let remaining = count - 1;
            if remaining == 0 {
                begin_sign_out();
            }
            Some(remaining)
        }
        Err(_) => None,
    }
}

fn begin_sign_out() {
    if USER_STATE
        .compare_exchange(
            XUSER_STATE_SIGNED_IN,
            XUSER_STATE_SIGNING_OUT,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return;
    }

    SIGNOUT_CALLBACKS_COMPLETE.store(false, Ordering::Release);
    bridge_info("最后一个 XUserHandle 已关闭；XUser 状态进入 SigningOut");
    dispatch_change_event(XUSER_CHANGE_EVENT_SIGNING_OUT);
    SIGNOUT_CALLBACKS_COMPLETE.store(true, Ordering::Release);
    try_complete_sign_out();
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
        bridge_info("XUser 注销延迟已完成；XUser 状态进入 SignedOut");
        dispatch_change_event(XUSER_CHANGE_EVENT_SIGNED_OUT);
    }
}

pub unsafe fn register_for_change_event(
    context: *mut c_void,
    callback: Option<XUserChangeEventCallback>,
    token: *mut XTaskQueueRegistrationToken,
) -> HResult {
    if token.is_null() || callback.is_none() {
        return E_POINTER;
    }
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
    S_OK
}

pub unsafe fn unregister_for_change_event(
    token: XTaskQueueRegistrationToken,
    wait: u8,
) -> u8 {
    if token.token == 0 {
        return 1;
    }
    let registration = {
        let entries = match registrations().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        entries.get(&token.token).cloned()
    };
    let Some(registration) = registration else {
        return 1;
    };

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
    let previous = SIGNOUT_DEFERRALS.fetch_update(
        Ordering::AcqRel,
        Ordering::Acquire,
        |count| (count != 0).then_some(count - 1),
    );
    if matches!(previous, Ok(1)) {
        try_complete_sign_out();
    }
}
