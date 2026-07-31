// SPDX-License-Identifier: GPL-3.0-or-later
//
// ABI ordering is adapted from WineGDK's xuser/provider definitions. This
// project targets Windows x64, where all vtable entries use the Microsoft x64
// calling convention.

use core::ffi::{c_char, c_void};

pub type HResult = i32;
pub const S_OK: HResult = 0;
pub const E_FAIL: HResult = 0x8000_4005_u32 as i32;
pub const E_POINTER: HResult = 0x8000_4003_u32 as i32;
pub const E_NOINTERFACE: HResult = 0x8000_4002_u32 as i32;
pub const E_NOTIMPL: HResult = 0x8000_4001_u32 as i32;
pub const E_INVALIDARG: HResult = 0x8007_0057_u32 as i32;
pub const E_NOT_SUFFICIENT_BUFFER: HResult = 0x8007_007a_u32 as i32;

pub const XUSER_STATE_SIGNED_IN: u32 = 0;
pub const XUSER_AGE_GROUP_UNKNOWN: u32 = 0;
pub const XUSER_AGE_GROUP_CHILD: u32 = 1;
pub const XUSER_AGE_GROUP_TEEN: u32 = 2;
pub const XUSER_AGE_GROUP_ADULT: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl Guid {
    pub const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
        Self {
            data1,
            data2,
            data3,
            data4,
        }
    }
}

pub const IID_IUNKNOWN: Guid = Guid::new(
    0x0000_0000,
    0x0000,
    0x0000,
    [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
);
pub const CLSID_XUSER_IMPL: Guid = Guid::new(
    0x01ac_d177,
    0x91f9,
    0x4763,
    [0xa3, 0x8e, 0xcc, 0xbb, 0x55, 0xce, 0x32, 0xe0],
);
pub const IID_IXUSER_BASE: Guid = CLSID_XUSER_IMPL;
pub const IID_IXUSER_ADD_WITH_UI: Guid = Guid::new(
    0xeb9b_f948,
    0x18dc,
    0x4d82,
    [0xbb, 0xcc, 0x40, 0xe0, 0xa8, 0x09, 0xc4, 0xc0],
);
pub const IID_IXUSER_MSA: Guid = Guid::new(
    0x1bf2_f8c5,
    0xd507,
    0x4e52,
    [0xbb, 0x05, 0xf7, 0x26, 0xd0, 0xe7, 0x11, 0x61],
);
pub const IID_IXUSER_STORE: Guid = Guid::new(
    0x0794_15e3,
    0x6727,
    0x437f,
    [0x8e, 0x9d, 0x8f, 0x8f, 0x9b, 0x24, 0x39, 0xf7],
);
pub const IID_IXUSER_PLATFORM: Guid = Guid::new(
    0x26f3_c674,
    0xa2fe,
    0x44fa,
    [0xb6, 0xc4, 0xa3, 0x23, 0xbc, 0x94, 0xff, 0x53],
);
pub const IID_IXUSER_SIGN_OUT: Guid = Guid::new(
    0x5131_d685,
    0x4394,
    0x4ee6,
    [0x8c, 0x18, 0xbf, 0xb5, 0xd4, 0xae, 0xf1, 0xff],
);
pub const IID_IXUSER_GAMERTAG: Guid = Guid::new(
    0xcef4_fac0,
    0x7676,
    0x4a94,
    [0xa1, 0x19, 0x4c, 0x43, 0xf9, 0xeb, 0x5b, 0x74],
);
pub const CLSID_XTHREADING_IMPL: Guid = Guid::new(
    0x073b_7dcb,
    0x1fcf,
    0x4030,
    [0x94, 0xbe, 0xe3, 0xc9, 0xeb, 0x62, 0x34, 0x28],
);
pub const IID_IXTHREADING_IMPL: Guid = CLSID_XTHREADING_IMPL;

pub type QueryApiImplFn =
    unsafe extern "system" fn(*const Guid, *const Guid, *mut *mut c_void) -> HResult;

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XAsyncOp {
    Begin = 0,
    DoWork = 1,
    GetResult = 2,
    Cancel = 3,
    Cleanup = 4,
}

#[repr(C)]
pub struct XAsyncBlock {
    pub queue: *mut c_void,
    pub context: *mut c_void,
    pub callback: Option<unsafe extern "system" fn(*mut XAsyncBlock)>,
    pub internal: [usize; 4],
}

#[repr(C)]
pub struct XAsyncProviderData {
    pub async_block: *mut XAsyncBlock,
    pub buffer_size: usize,
    pub buffer: *mut c_void,
    pub context: *mut c_void,
}

pub type XAsyncProvider =
    unsafe extern "system" fn(XAsyncOp, *const XAsyncProviderData) -> HResult;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XUserLocalId {
    pub value: u64,
}

pub type XUserHandle = *mut c_void;

#[repr(C)]
pub struct XUserVtable {
    pub slots: [usize; 50],
}

#[repr(C)]
pub struct XUserGamertagVtable {
    pub slots: [usize; 4],
}

#[repr(C)]
pub struct TokenData {
    pub token_size: usize,
    pub signature_size: usize,
    pub token: *const c_char,
    pub signature: *const c_char,
}

#[repr(C)]
pub struct TokenHeader {
    pub name: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub struct TokenUtf16Data {
    pub token_count: usize,
    pub signature_count: usize,
    pub token: *const u16,
    pub signature: *const u16,
}

#[repr(C)]
pub struct TokenUtf16Header {
    pub name: *const u16,
    pub value: *const u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xuser_vtable_layout_is_stable() {
        assert_eq!(
            core::mem::size_of::<XUserVtable>(),
            50 * core::mem::size_of::<usize>()
        );
        assert_eq!(
            core::mem::size_of::<XUserGamertagVtable>(),
            4 * core::mem::size_of::<usize>()
        );
    }

    #[test]
    fn async_provider_data_is_pointer_aligned() {
        assert_eq!(
            core::mem::size_of::<XAsyncProviderData>(),
            4 * core::mem::size_of::<usize>()
        );
    }
}
