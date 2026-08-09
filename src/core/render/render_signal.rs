use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::core::runtime_ready::{self, ReadyLevel};
use crate::runtime::foundation::logging;

static INSTALLED: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
static LEGACY_FRAME_WARNING_LOGGED: AtomicBool = AtomicBool::new(false);

/// Compatibility entry point retained for existing loader code.
///
/// BLoader no longer creates a dummy HWND, DXGI factory, D3D12 device,
/// command queue or swapchain and no longer hooks IDXGISwapChain::Present.
/// Delayed native Mod loading is driven by `runtime_ready` instead.
pub fn install() -> bool {
    if !INSTALLED.swap(true, Ordering::AcqRel) {
        logging::scoped_info_message(
            "runtime-ready",
            "Legacy render_signal API mapped to non-graphics runtime readiness; D3D/DXGI/Present hooks are disabled.",
        );
    }
    true
}

pub fn is_installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

/// Legacy compatibility value.
///
/// A value of 1 means the stable-window runtime condition has been reached.
/// It is no longer a GPU frame counter.
pub fn frame_count() -> u64 {
    u64::from(READY.load(Ordering::Acquire))
}

/// Legacy compatibility wait used by existing `hot-native` / `hot-inject`
/// manifests. `target` is intentionally not interpreted as a real frame count.
/// Any positive target now means: wait until Minecraft has crossed OEP and its
/// visible process-owned window has remained stable for the readiness interval.
pub fn wait_for_frame(target: u64, timeout: Duration) -> bool {
    if target == 0 || READY.load(Ordering::Acquire) {
        return true;
    }

    install();

    if target > 1 && !LEGACY_FRAME_WARNING_LOGGED.swap(true, Ordering::AcqRel) {
        logging::scoped_warn_message(
            "runtime-ready",
            "inject_min_frames/render frame readiness is deprecated; legacy frame waits are mapped to stable-window readiness without touching the graphics pipeline.",
        );
    }

    let ready = runtime_ready::wait_for(ReadyLevel::StableWindow, timeout);
    if ready {
        READY.store(true, Ordering::Release);
    }
    ready
}
