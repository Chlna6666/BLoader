use std::ffi::c_void;
use std::mem;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use minhook::MinHook;
use windows::core::{Interface, BOOL};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D12::{
    D3D12CreateDevice, ID3D12CommandQueue, ID3D12Device, D3D12_COMMAND_LIST_TYPE_DIRECT,
    D3D12_COMMAND_QUEUE_DESC, D3D12_COMMAND_QUEUE_FLAG_NONE,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIFactory2, IDXGISwapChain, IDXGISwapChain3,
    DXGI_CREATE_FACTORY_FLAGS, DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

use crate::runtime::foundation::logging;

type PresentFn = unsafe extern "system" fn(
    this: IDXGISwapChain3,
    sync_interval: u32,
    flags: u32,
) -> windows::core::HRESULT;

static INSTALLED: AtomicBool = AtomicBool::new(false);
static ORIGINAL_PRESENT: AtomicUsize = AtomicUsize::new(0);
static FRAME_COUNT: AtomicU64 = AtomicU64::new(0);
static FIRST_FRAME_LOGGED: AtomicBool = AtomicBool::new(false);

pub fn install() -> bool {
    if INSTALLED.load(Ordering::Acquire) {
        return true;
    }

    let Ok(target) = resolve_present_target() else {
        logging::warn_message("Render signal: failed to resolve IDXGISwapChain::Present.");
        return false;
    };

    let original = match unsafe {
        MinHook::create_hook(target as *mut c_void, detour_present as *mut c_void)
    } {
        Ok(original) => original,
        Err(error) => {
            logging::warn_message(&format!(
                "Render signal: failed to create Present hook: {error:?}"
            ));
            return false;
        }
    };
    ORIGINAL_PRESENT.store(original as usize, Ordering::Release);

    if let Err(error) = unsafe { MinHook::enable_all_hooks() } {
        ORIGINAL_PRESENT.store(0, Ordering::Release);
        logging::warn_message(&format!(
            "Render signal: failed to enable Present hook: {error:?}"
        ));
        return false;
    }

    INSTALLED.store(true, Ordering::Release);
    logging::info_message(
        "Render signal hook armed: Present observer only; no ArcUI rendering or input capture.",
    );
    true
}

pub fn is_installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

pub fn frame_count() -> u64 {
    FRAME_COUNT.load(Ordering::Acquire)
}

pub fn wait_for_frame(target: u64, timeout: Duration) -> bool {
    if target == 0 || frame_count() >= target {
        return true;
    }
    if !is_installed() {
        return false;
    }

    let start = Instant::now();
    while start.elapsed() < timeout {
        if frame_count() >= target {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    frame_count() >= target
}

unsafe extern "system" fn detour_present(
    swap_chain: IDXGISwapChain3,
    sync_interval: u32,
    flags: u32,
) -> windows::core::HRESULT {
    let frame = FRAME_COUNT.fetch_add(1, Ordering::AcqRel) + 1;
    if frame == 1 && !FIRST_FRAME_LOGGED.swap(true, Ordering::AcqRel) {
        logging::info_message("Graphics pipeline ready: first real Present observed.");
    }

    // Preserve the lightweight render-frame event used by Mods as a readiness signal.
    crate::bl::host::dispatch_render_frame();

    let original_raw = ORIGINAL_PRESENT.load(Ordering::Acquire);
    if original_raw == 0 {
        return windows::core::HRESULT(0);
    }
    let original: PresentFn = mem::transmute(original_raw);
    original(swap_chain, sync_interval, flags)
}

fn resolve_present_target() -> windows::core::Result<PresentFn> {
    let dummy = crate::bl_dummy_hwnd::dummy_hwnd();
    let result = (|| {
        let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) }?;
        let adapter = unsafe { factory.EnumAdapters(0) }?;
        let device: ID3D12Device =
            unsafe { out_ptr(|value| D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, value)) }?;
        let queue: ID3D12CommandQueue = unsafe {
            device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                Priority: 0,
                Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
                NodeMask: 0,
            })
        }?;

        let swap_chain: IDXGISwapChain3 = unsafe {
            factory.CreateSwapChainForHwnd(
                &queue,
                dummy,
                &DXGI_SWAP_CHAIN_DESC1 {
                    Width: 64,
                    Height: 64,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    Stereo: BOOL(0),
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: 2,
                    Scaling: DXGI_SCALING_NONE,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                    AlphaMode: Default::default(),
                    Flags: DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH.0 as u32,
                },
                None,
                None,
            )?
        }
        .cast()?;

        let base: IDXGISwapChain = swap_chain.cast()?;
        Ok(unsafe { mem::transmute(base.vtable().Present) })
    })();

    unsafe {
        let _ = DestroyWindow(dummy);
    }
    result
}

unsafe fn out_ptr<T, F>(mut f: F) -> windows::core::Result<T>
where
    F: FnMut(&mut Option<T>) -> windows::core::Result<()>,
{
    let mut value = None;
    f(&mut value)?;
    value.ok_or_else(|| windows::core::Error::from(windows::core::HRESULT(0x80004005u32 as i32)))
}
