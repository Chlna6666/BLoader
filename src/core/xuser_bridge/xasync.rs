// SPDX-License-Identifier: GPL-3.0-or-later

use core::ffi::{c_char, c_void};

use super::{
    abi::{
        CLSID_XTHREADING_IMPL, E_FAIL, E_POINTER, HResult, IID_IXTHREADING_IMPL, XAsyncBlock,
        XAsyncProvider,
    },
    call_original_query,
};

#[repr(C)]
struct XThreadingInterface {
    vtable: *const XThreadingVtable,
}

#[repr(C)]
struct XThreadingVtable {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut XThreadingInterface) -> u32,
    async_get_status: usize,
    async_get_result_size: unsafe extern "system" fn(
        *mut XThreadingInterface,
        *mut XAsyncBlock,
        *mut usize,
    ) -> HResult,
    async_cancel: usize,
    async_run: usize,
    async_begin: unsafe extern "system" fn(
        *mut XThreadingInterface,
        *mut XAsyncBlock,
        *mut c_void,
        *const c_void,
        *const c_char,
        XAsyncProvider,
    ) -> HResult,
    padding: usize,
    async_schedule:
        unsafe extern "system" fn(*mut XThreadingInterface, *mut XAsyncBlock, u32) -> HResult,
    async_complete:
        unsafe extern "system" fn(*mut XThreadingInterface, *mut XAsyncBlock, HResult, usize),
    async_get_result: unsafe extern "system" fn(
        *mut XThreadingInterface,
        *mut XAsyncBlock,
        *const c_void,
        usize,
        *mut c_void,
        *mut usize,
    ) -> HResult,
}

struct ThreadingHandle(*mut XThreadingInterface);

impl ThreadingHandle {
    fn acquire() -> Result<Self, HResult> {
        let mut interface = core::ptr::null_mut();
        let status = unsafe {
            call_original_query(
                &CLSID_XTHREADING_IMPL,
                &IID_IXTHREADING_IMPL,
                &mut interface,
            )
        };
        if status < 0 {
            return Err(status);
        }
        if interface.is_null() {
            return Err(E_POINTER);
        }
        Ok(Self(interface.cast()))
    }

    fn vtable(&self) -> &XThreadingVtable {
        unsafe { &*(*self.0).vtable }
    }
}

impl Drop for ThreadingHandle {
    fn drop(&mut self) {
        unsafe {
            (self.vtable().release)(self.0);
        }
    }
}

pub unsafe fn begin(
    async_block: *mut XAsyncBlock,
    context: *mut c_void,
    identity: *const c_void,
    identity_name: *const c_char,
    provider: XAsyncProvider,
) -> HResult {
    if async_block.is_null() || identity.is_null() || identity_name.is_null() {
        return E_POINTER;
    }
    let Ok(threading) = ThreadingHandle::acquire() else {
        return E_FAIL;
    };
    unsafe {
        (threading.vtable().async_begin)(
            threading.0,
            async_block,
            context,
            identity,
            identity_name,
            provider,
        )
    }
}

pub unsafe fn schedule(async_block: *mut XAsyncBlock, delay_ms: u32) -> HResult {
    if async_block.is_null() {
        return E_POINTER;
    }
    let Ok(threading) = ThreadingHandle::acquire() else {
        return E_FAIL;
    };
    unsafe { (threading.vtable().async_schedule)(threading.0, async_block, delay_ms) }
}

pub unsafe fn complete(async_block: *mut XAsyncBlock, result: HResult, required_size: usize) {
    if async_block.is_null() {
        return;
    }
    if let Ok(threading) = ThreadingHandle::acquire() {
        unsafe {
            (threading.vtable().async_complete)(threading.0, async_block, result, required_size);
        }
    }
}

pub unsafe fn get_result_size(async_block: *mut XAsyncBlock, size: *mut usize) -> HResult {
    if async_block.is_null() || size.is_null() {
        return E_POINTER;
    }
    let Ok(threading) = ThreadingHandle::acquire() else {
        return E_FAIL;
    };
    unsafe { (threading.vtable().async_get_result_size)(threading.0, async_block, size) }
}

pub unsafe fn get_result(
    async_block: *mut XAsyncBlock,
    identity: *const c_void,
    buffer_size: usize,
    buffer: *mut c_void,
    used: *mut usize,
) -> HResult {
    if async_block.is_null() || identity.is_null() || (buffer_size != 0 && buffer.is_null()) {
        return E_POINTER;
    }
    let Ok(threading) = ThreadingHandle::acquire() else {
        return E_FAIL;
    };
    unsafe {
        (threading.vtable().async_get_result)(
            threading.0,
            async_block,
            identity,
            buffer_size,
            buffer,
            used,
        )
    }
}
