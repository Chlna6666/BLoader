use std::cell::Cell;
use std::ffi::c_void;
use std::mem::{self, ManuallyDrop};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use arcui_core::{Color, DrawCommandKind, DrawPrimitive, Rect, Vec2};
use arcui_platform_win32::{PlatformEvent, Win32Platform};
use minhook::MinHook;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_POINT_2U, D2D_RECT_F, D2D_RECT_U, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED,
    D2D1_BORDER_MODE_HARD, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1GaussianBlur, D2D1_ANTIALIAS_MODE_ALIASED, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_MULTI_THREADED, D2D1_GAUSSIANBLUR_PROP_BORDER_MODE,
    D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_PROPERTY_TYPE_ENUM, D2D1_PROPERTY_TYPE_FLOAT, D2D1_ROUNDED_RECT, D2D1CreateFactory,
    ID2D1Bitmap1, ID2D1Brush, ID2D1Device, ID2D1DeviceContext, ID2D1Effect, ID2D1Factory1,
    ID2D1Image, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_CREATE_DEVICE_BGRA_SUPPORT, ID3D11Device, ID3D11DeviceContext,
    ID3D11Resource,
};
use windows::Win32::Graphics::Direct3D11on12::{
    D3D11_RESOURCE_FLAGS, D3D11On12CreateDevice, ID3D11On12Device,
};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_QUEUE_DESC, D3D12_COMMAND_QUEUE_FLAG_NONE,
    D3D12_FENCE_FLAG_NONE, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
    D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, D3D12_RESOURCE_BARRIER_FLAG_NONE,
    D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_STATE_PRESENT,
    D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATES, D3D12_RESOURCE_TRANSITION_BARRIER,
    D3D12CreateDevice, ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue, ID3D12Device,
    ID3D12Fence, ID3D12GraphicsCommandList, ID3D12Resource,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain,
    IDXGISwapChain3,
};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Threading::{CreateEventW, GetCurrentProcessId, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, ClipCursor, DefWindowProcW, GWLP_WNDPROC, GetForegroundWindow,
    GetWindowThreadProcessId, HTCLIENT, IDC_ARROW, LoadCursorW, MA_ACTIVATEANDEAT, PostMessageW,
    SetCursor, SetCursorPos, SetWindowLongPtrW, ShowCursor, WM_INPUT, WM_KEYUP, WM_LBUTTONUP,
    WM_MBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_RBUTTONUP, WM_SETCURSOR,
    WM_SYSKEYUP, WM_XBUTTONUP,
};
use windows::core::{BOOL, IUnknown, Interface, w};
use windows_numerics::Vector2;

#[link(name = "user32")]
unsafe extern "system" {
    fn FindWindowW(lp_class_name: *const u16, lp_window_name: *const u16) -> HWND;
    fn FindWindowExW(
        parent: HWND,
        child_after: HWND,
        class_name: *const u16,
        window_name: *const u16,
    ) -> HWND;
}

use crate::dummy_hwnd;
use crate::{DrawDataCallback, Dx12RenderCallback};

type PresentFn = unsafe extern "system" fn(
    this: IDXGISwapChain3,
    sync_interval: u32,
    flags: u32,
) -> windows::core::HRESULT;
type ExecuteCommandListsFn = unsafe extern "system" fn(
    this: ID3D12CommandQueue,
    num_command_lists: u32,
    command_lists: *mut ID3D12CommandList,
);
type ResizeBuffersFn = unsafe extern "system" fn(
    this: IDXGISwapChain3,
    buffer_count: u32,
    width: u32,
    height: u32,
    new_format: DXGI_FORMAT,
    flags: u32,
) -> windows::core::HRESULT;
type ResizeBuffers1Fn = unsafe extern "system" fn(
    this: IDXGISwapChain3,
    buffer_count: u32,
    width: u32,
    height: u32,
    new_format: DXGI_FORMAT,
    flags: u32,
    creation_node_mask: *const u32,
    present_queue: *const *const IUnknown,
) -> windows::core::HRESULT;

struct HookTargets {
    present: PresentFn,
    execute_command_lists: ExecuteCommandListsFn,
    resize_buffers: ResizeBuffersFn,
    resize_buffers1: ResizeBuffers1Fn,
}

struct InitContext {
    swap_chain: Option<IDXGISwapChain3>,
    command_queue: Option<ID3D12CommandQueue>,
}

struct WrappedBackBuffer {
    d3d12_resource: ID3D12Resource,
    d3d11_resource: ID3D11Resource,
    d2d_target: ID2D1Bitmap1,
}

struct FrameState {
    swap_chain: IDXGISwapChain3,
    device12: ID3D12Device,
    command_queue: ID3D12CommandQueue,
    command_allocator: ID3D12CommandAllocator,
    command_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_value: u64,
    fence_event: isize,
    d3d11_context: ID3D11DeviceContext,
    d3d11on12_device: ID3D11On12Device,
    d2d_context: ID2D1DeviceContext,
    text_format: IDWriteTextFormat,
    brush: ID2D1SolidColorBrush,
    blur_effect: Option<ID2D1Effect>,
    blur_snapshot: Option<ID2D1Bitmap1>,
    blur_snapshot_size: (u32, u32),
    back_buffers: Vec<WrappedBackBuffer>,
}

#[derive(Clone, Copy)]
struct LoaderBlurState {
    rect: Rect,
    visibility: f32,
}

static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
static ORIGINAL_PRESENT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_EXECUTE_COMMAND_LISTS: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_RESIZE_BUFFERS: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_RESIZE_BUFFERS1: AtomicUsize = AtomicUsize::new(0);
static INIT_CONTEXT: OnceLock<Mutex<InitContext>> = OnceLock::new();
static FRAME_STATE: OnceLock<Mutex<Option<FrameState>>> = OnceLock::new();
static OVERLAY_STATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static DRAW_DATA_CALLBACK: OnceLock<DrawDataCallback> = OnceLock::new();
static DX12_RENDER_CALLBACK: OnceLock<Dx12RenderCallback> = OnceLock::new();
static PLATFORM: OnceLock<Mutex<Win32Platform>> = OnceLock::new();
static LOADER_BLUR_STATE: OnceLock<Mutex<Option<LoaderBlurState>>> = OnceLock::new();
static CAPTURE_REGION: OnceLock<Mutex<Option<Rect>>> = OnceLock::new();
pub static HOST_HWND_RAW: AtomicIsize = AtomicIsize::new(0);
static ORIGINAL_WNDPROC: AtomicIsize = AtomicIsize::new(0);
static WNDPROC_SUBCLASSED: AtomicBool = AtomicBool::new(false);
static CAPTURE_INPUT: AtomicBool = AtomicBool::new(false);
static CURSOR_VISIBILITY_OWNED: AtomicBool = AtomicBool::new(false);
static CURSOR_WARP_GENERATION: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static CURSOR_WARP_BYPASS: Cell<bool> = const { Cell::new(false) };
}
static RESIZE_PENDING: AtomicBool = AtomicBool::new(false);
static RESIZE_SKIP_FRAMES: AtomicU32 = AtomicU32::new(0);
static RESIZE_LAST_SIZE: AtomicU64 = AtomicU64::new(0);
static RESIZE_STABLE_FRAMES: AtomicU32 = AtomicU32::new(0);
static LOADER_BLUR_STRENGTH_BITS: AtomicU32 = AtomicU32::new(f32::to_bits(1.0));
const RESIZE_SKIP_FRAME_COUNT: u32 = 10;
const RESIZE_REQUIRED_STABLE_FRAMES: u32 = 8;
pub fn install(draw_data_callback: DrawDataCallback) -> Result<(), String> {
    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let targets = get_target_addrs()?;

    let present = unsafe {
        MinHook::create_hook(
            targets.present as *mut c_void,
            detour_present as *mut c_void,
        )
    }
    .map_err(|e| format!("{e:?}"))?;
    ORIGINAL_PRESENT.store(present as usize, Ordering::SeqCst);

    let execute = unsafe {
        MinHook::create_hook(
            targets.execute_command_lists as *mut c_void,
            detour_execute_command_lists as *mut c_void,
        )
    }
    .map_err(|e| format!("{e:?}"))?;
    ORIGINAL_EXECUTE_COMMAND_LISTS.store(execute as usize, Ordering::SeqCst);

    let resize = unsafe {
        MinHook::create_hook(
            targets.resize_buffers as *mut c_void,
            detour_resize_buffers as *mut c_void,
        )
    }
    .map_err(|e| format!("{e:?}"))?;
    ORIGINAL_RESIZE_BUFFERS.store(resize as usize, Ordering::SeqCst);

    let resize1 = unsafe {
        MinHook::create_hook(
            targets.resize_buffers1 as *mut c_void,
            detour_resize_buffers1 as *mut c_void,
        )
    }
    .map_err(|e| format!("{e:?}"))?;
    ORIGINAL_RESIZE_BUFFERS1.store(resize1 as usize, Ordering::SeqCst);

    unsafe { MinHook::enable_all_hooks() }.map_err(|e| format!("{e:?}"))?;

    let _ = INIT_CONTEXT.set(Mutex::new(InitContext {
        swap_chain: None,
        command_queue: None,
    }));
    let _ = FRAME_STATE.set(Mutex::new(None));
    let _ = OVERLAY_STATE_LOCK.set(Mutex::new(()));
    let _ = DRAW_DATA_CALLBACK.set(draw_data_callback);
    let _ = PLATFORM.set(Mutex::new(Win32Platform::new()));
    Ok(())
}

pub fn set_capture_input(enabled: bool) {
    let previous = CAPTURE_INPUT.swap(enabled, Ordering::AcqRel);
    if enabled && !previous {
        release_host_input_state();
        // The game may still own a centered/hidden cursor during the same frame
        // in which the panel opens. Wait for ArcUI's first interactive frame,
        // then place the pointer inside the centered panel. This mirrors the
        // delayed cursor hand-off used by robust injected overlays and prevents
        // the first click from remaining bound to Minecraft underneath ArcUI.
        schedule_cursor_warp(Duration::from_millis(100));
    } else if !enabled && previous {
        // Cancel a delayed warp when the panel was closed again within 100 ms.
        CURSOR_WARP_GENERATION.fetch_add(1, Ordering::AcqRel);
    }

    if enabled {
        let _ = unsafe { ClipCursor(None) };
        if !CURSOR_VISIBILITY_OWNED.swap(true, Ordering::AcqRel) {
            unsafe { while ShowCursor(true) < 0 {} }
        }
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();
        let _ = unsafe { SetCursor(Some(cursor)) };
    } else if CURSOR_VISIBILITY_OWNED.swap(false, Ordering::AcqRel) {
        unsafe { while ShowCursor(false) >= 0 {} }
    }
}

pub fn is_capturing_input() -> bool {
    CAPTURE_INPUT.load(Ordering::Acquire)
}

/// Actual Minecraft input window used by the ArcUI WndProc barrier.
/// Low-level mouse hooks are observation-only; ArcUI receives normal Win32
/// messages from this HWND while in-process input readers are neutralized.
pub fn host_hwnd_raw() -> isize {
    HOST_HWND_RAW.load(Ordering::Acquire)
}

/// True only on the thread performing ArcUI's intentional delayed cursor hand-off.
/// The loader's SetCursorPos detour uses this to distinguish the one permitted
/// panel-centering call from Minecraft's continuous camera recentering calls.
pub fn cursor_warp_bypass_active() -> bool {
    CURSOR_WARP_BYPASS.with(Cell::get)
}

pub fn set_capture_region(region: Option<Rect>) {
    let clamped = region.filter(|rect| rect.width() > 1.0 && rect.height() > 1.0);
    let mut guard = capture_region().lock().unwrap_or_else(|e| e.into_inner());
    *guard = clamped;
}

pub fn set_loader_blur_region(region: Option<Rect>, visibility: f32) {
    let mut guard = loader_blur_state()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = region
        .filter(|rect| rect.width() > 1.0 && rect.height() > 1.0 && visibility > 0.01)
        .map(|rect| LoaderBlurState {
            rect,
            visibility: visibility.clamp(0.0, 1.0),
        });
}

pub fn set_loader_blur_strength(strength: f32) {
    LOADER_BLUR_STRENGTH_BITS.store(strength.clamp(0.0, 2.4).to_bits(), Ordering::Release);
}

pub fn set_dx12_render_callback(callback: Dx12RenderCallback) {
    let _ = DX12_RENDER_CALLBACK.set(callback);
}

pub(crate) fn dispatch_platform_event(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) {
    if let Some(platform_slot) = PLATFORM.get() {
        if let Ok(mut platform) = platform_slot.lock() {
            platform.handle_event(PlatformEvent {
                hwnd,
                message: msg,
                wparam,
                lparam,
            });
        }
    }
}

unsafe extern "system" fn detour_present(
    swap_chain: IDXGISwapChain3,
    sync_interval: u32,
    flags: u32,
) -> windows::core::HRESULT {
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        if let Some(slot) = INIT_CONTEXT.get() {
            if let Ok(mut guard) = slot.lock() {
                if guard.swap_chain.is_none() {
                    guard.swap_chain = Some(swap_chain.clone());
                }
            }
        }

        let _ = ensure_window_hook(&swap_chain);

        let state_lock = OVERLAY_STATE_LOCK
            .get()
            .expect("overlay state lock initialized");
        if let Ok(_overlay_guard) = state_lock.try_lock() {
            if should_render_this_frame(&swap_chain) {
                let _ = render_overlay(&swap_chain);
            }
        }
    }));

    let original: PresentFn = unsafe { mem::transmute(ORIGINAL_PRESENT.load(Ordering::SeqCst)) };
    unsafe { original(swap_chain, sync_interval, flags) }
}

unsafe extern "system" fn detour_execute_command_lists(
    command_queue: ID3D12CommandQueue,
    num_command_lists: u32,
    command_lists: *mut ID3D12CommandList,
) {
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        if let Some(slot) = INIT_CONTEXT.get() {
            if let Ok(mut guard) = slot.lock() {
                if guard.command_queue.is_none() {
                    if let Some(swap_chain) = &guard.swap_chain {
                        if command_queue_matches_swap_chain(swap_chain, &command_queue) {
                            guard.command_queue = Some(command_queue.clone());
                        }
                    }
                }
            }
        }
    }));

    let original: ExecuteCommandListsFn =
        unsafe { mem::transmute(ORIGINAL_EXECUTE_COMMAND_LISTS.load(Ordering::SeqCst)) };
    unsafe { original(command_queue, num_command_lists, command_lists) };
}

unsafe extern "system" fn detour_resize_buffers(
    swap_chain: IDXGISwapChain3,
    buffer_count: u32,
    width: u32,
    height: u32,
    new_format: DXGI_FORMAT,
    flags: u32,
) -> windows::core::HRESULT {
    let state_lock = OVERLAY_STATE_LOCK
        .get()
        .expect("overlay state lock initialized");
    let _overlay_guard = state_lock.lock().unwrap_or_else(|e| e.into_inner());

    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        begin_resize_transition(width, height);
    }));

    let original: ResizeBuffersFn =
        unsafe { mem::transmute(ORIGINAL_RESIZE_BUFFERS.load(Ordering::SeqCst)) };
    unsafe { original(swap_chain, buffer_count, width, height, new_format, flags) }
}

unsafe extern "system" fn detour_resize_buffers1(
    swap_chain: IDXGISwapChain3,
    buffer_count: u32,
    width: u32,
    height: u32,
    new_format: DXGI_FORMAT,
    flags: u32,
    creation_node_mask: *const u32,
    present_queue: *const *const IUnknown,
) -> windows::core::HRESULT {
    let state_lock = OVERLAY_STATE_LOCK
        .get()
        .expect("overlay state lock initialized");
    let _overlay_guard = state_lock.lock().unwrap_or_else(|e| e.into_inner());

    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        begin_resize_transition(width, height);
    }));

    let original: ResizeBuffers1Fn =
        unsafe { mem::transmute(ORIGINAL_RESIZE_BUFFERS1.load(Ordering::SeqCst)) };
    unsafe {
        original(
            swap_chain,
            buffer_count,
            width,
            height,
            new_format,
            flags,
            creation_node_mask,
            present_queue,
        )
    }
}

unsafe extern "system" fn host_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        dispatch_platform_event(hwnd, msg, wparam, lparam);
    }));

    if should_consume_host_message(hwnd, msg, lparam) {
        if msg == WM_SETCURSOR {
            let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();
            let _ = unsafe { SetCursor(Some(cursor)) };
        }
        if msg == WM_NCHITTEST {
            return LRESULT(HTCLIENT as isize);
        }
        if msg == WM_MOUSEACTIVATE {
            return LRESULT(MA_ACTIVATEANDEAT as isize);
        }
        return LRESULT(1);
    }

    let original = ORIGINAL_WNDPROC.load(Ordering::Acquire);
    if original != 0 {
        unsafe { CallWindowProcW(Some(mem::transmute(original)), hwnd, msg, wparam, lparam) }
    } else {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
}

fn should_consume_host_message(_hwnd: HWND, msg: u32, _lparam: LPARAM) -> bool {
    if !CAPTURE_INPUT.load(Ordering::Acquire) {
        return false;
    }

    // ChiyanMap-style full UI barrier: ArcUI already received the message at
    // the start of host_wnd_proc, so every mouse/raw-input message is now
    // stopped before Minecraft's original WndProc. Do not depend on the current
    // pointer coordinate; the cursor can still be outside the animated panel
    // during the first frame after opening.
    matches!(msg, WM_INPUT | 0x00FE) || is_keyboard_message(msg) || is_pointer_message(msg)
}

fn is_keyboard_message(msg: u32) -> bool {
    matches!(msg, 0x0100..=0x0109)
}

fn is_pointer_message(msg: u32) -> bool {
    matches!(
        msg,
        0x0200..=0x020F
            | 0x0240..=0x0255
            | WM_INPUT
            | 0x00FE
            | WM_SETCURSOR
            | WM_NCHITTEST
            | WM_MOUSEACTIVATE
    )
}

fn render_overlay(swap_chain: &IDXGISwapChain3) -> windows::core::Result<()> {
    let width = unsafe { swap_chain.GetDesc1()?.Width };
    let height = unsafe { swap_chain.GetDesc1()?.Height };
    if width == 0 || height == 0 {
        return Ok(());
    }

    let hwnd = current_host_hwnd().unwrap_or_default();
    let input = if let Some(platform_slot) = PLATFORM.get() {
        let mut platform = platform_slot.lock().unwrap_or_else(|e| e.into_inner());
        platform.set_display_size(width, height);
        platform.take_snapshot(hwnd)
    } else {
        arcui_core::InputSnapshot {
            display_size: arcui_core::Vec2::new(width as f32, height as f32),
            ..Default::default()
        }
    };

    let draw_data = DRAW_DATA_CALLBACK
        .get()
        .copied()
        .expect("draw data callback initialized")(&input);
    let has_blur = loader_blur_state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
        && current_loader_blur_strength() > f32::EPSILON;
    if !has_blur && !draw_data_has_work(&draw_data) {
        return Ok(());
    }

    let frame_slot = FRAME_STATE.get().expect("frame state initialized");
    let mut frame_guard = frame_slot.lock().unwrap_or_else(|e| e.into_inner());

    if frame_guard
        .as_ref()
        .is_some_and(|frame| !frame_state_matches_swap_chain(frame, swap_chain))
    {
        if let Some(mut stale_frame) = frame_guard.take() {
            release_all_resources(&mut stale_frame);
        }
    }

    if frame_guard.is_none() {
        let init_slot = INIT_CONTEXT.get().expect("init context initialized");
        let init_guard = init_slot.lock().unwrap_or_else(|e| e.into_inner());
        let Some(command_queue) = init_guard.command_queue.clone() else {
            return Ok(());
        };
        *frame_guard = Some(create_frame_state(swap_chain, &command_queue)?);
    }

    let Some(frame) = frame_guard.as_mut() else {
        return Ok(());
    };

    let back_buffer_index = unsafe { swap_chain.GetCurrentBackBufferIndex() as usize };
    let Some(back_buffer) = frame.back_buffers.get(back_buffer_index) else {
        return Ok(());
    };
    let wrapped_resource = back_buffer.d3d11_resource.clone();
    let d2d_target = back_buffer.d2d_target.clone();

    let wrapped = [Some(wrapped_resource)];
    unsafe {
        frame.d3d11on12_device.AcquireWrappedResources(&wrapped);
        frame.d2d_context.SetTarget(&d2d_target);
        frame.d2d_context.BeginDraw();
    }

    let _ = render_loader_blur(frame, width, height);

    for list in &draw_data.lists {
        for cmd in &list.commands {
            unsafe {
                frame
                    .d2d_context
                    .PushAxisAlignedClip(&to_d2d_rect(cmd.clip_rect), D2D1_ANTIALIAS_MODE_ALIASED);
                match &cmd.kind {
                    DrawCommandKind::Primitive(primitive) => {
                        let rect = to_d2d_rect(cmd.bounds);
                        let color = list
                            .indices
                            .get(cmd.index_start as usize)
                            .and_then(|index| list.vertices.get(*index as usize))
                            .map(|vertex| unpack_color(vertex.color))
                            .unwrap_or_else(|| unpack_color(Color::WHITE.0));
                        frame.brush.SetColor(&color);
                        match primitive {
                            DrawPrimitive::Rect => {
                                frame
                                    .d2d_context
                                    .FillRectangle(&rect, &frame.brush.cast::<ID2D1Brush>()?);
                            }
                            DrawPrimitive::RoundedRect { radius } => {
                                frame.d2d_context.FillRoundedRectangle(
                                    &D2D1_ROUNDED_RECT {
                                        rect,
                                        radiusX: *radius,
                                        radiusY: *radius,
                                    },
                                    &frame.brush.cast::<ID2D1Brush>()?,
                                );
                            }
                        }
                    }
                    DrawCommandKind::Text { text, color } => {
                        let wide: Vec<u16> = text.encode_utf16().collect();
                        if !wide.is_empty() {
                            frame.brush.SetColor(&unpack_color(color.0));
                            let text_rect = to_d2d_rect(cmd.bounds);
                            frame.d2d_context.DrawText(
                                &wide,
                                &frame.text_format,
                                &text_rect,
                                &frame.brush.cast::<ID2D1Brush>()?,
                                Default::default(),
                                DWRITE_MEASURING_MODE_NATURAL,
                            );
                        }
                    }
                    DrawCommandKind::VectorIcon { icon, color } => {
                        frame.brush.SetColor(&unpack_color(color.0));
                        let scale = (cmd.bounds.width().min(cmd.bounds.height())
                            / icon.viewport.max(1.0))
                        .max(0.0001);
                        let stroke = (icon.stroke_width * scale).max(1.0);
                        for segment in icon.segments {
                            frame.d2d_context.DrawLine(
                                Vector2 {
                                    X: cmd.bounds.min.x + segment.start.x * scale,
                                    Y: cmd.bounds.min.y + segment.start.y * scale,
                                },
                                Vector2 {
                                    X: cmd.bounds.min.x + segment.end.x * scale,
                                    Y: cmd.bounds.min.y + segment.end.y * scale,
                                },
                                &frame.brush.cast::<ID2D1Brush>()?,
                                stroke,
                                None,
                            );
                        }
                    }
                }
                frame.d2d_context.PopAxisAlignedClip();
            }
        }
    }

    let end_draw_result = unsafe { frame.d2d_context.EndDraw(None, None) };
    unsafe {
        let _ = frame.d2d_context.SetTarget(None::<&ID2D1Image>);
        frame.d3d11on12_device.ReleaseWrappedResources(&wrapped);
        frame.d3d11_context.Flush();
    }
    end_draw_result?;

    Ok(())
}

fn render_loader_blur(
    frame: &mut FrameState,
    width: u32,
    height: u32,
) -> windows::core::Result<()> {
    let blur_state = {
        let guard = loader_blur_state()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard
    };
    let Some(blur_state) = blur_state else {
        return Ok(());
    };

    let draw_rect = clamp_rect_to_output(blur_state.rect, width as f32, height as f32);
    if draw_rect.width() < 2.0 || draw_rect.height() < 2.0 {
        return Ok(());
    }

    let blur_scale = current_loader_blur_strength();
    if blur_scale <= f32::EPSILON {
        return Ok(());
    }
    let blur_sigma = ((7.5 + blur_state.visibility * 10.5) * blur_scale).clamp(6.0, 36.0);
    let capture_padding = (blur_sigma * 3.0).ceil().max(28.0);
    let capture_rect = expanded_capture_rect(draw_rect, capture_padding, width, height);
    let capture_width = capture_rect.right.saturating_sub(capture_rect.left);
    let capture_height = capture_rect.bottom.saturating_sub(capture_rect.top);
    if capture_width < 2 || capture_height < 2 {
        return Ok(());
    }

    let snapshot = ensure_blur_snapshot(frame, capture_width, capture_height)?;
    let source_rect = D2D_RECT_U {
        left: capture_rect.left,
        top: capture_rect.top,
        right: capture_rect.right,
        bottom: capture_rect.bottom,
    };
    let origin = D2D_POINT_2U { x: 0, y: 0 };
    unsafe {
        snapshot.CopyFromRenderTarget(Some(&origin), &frame.d2d_context, Some(&source_rect))?;
    }

    let blur_effect = ensure_blur_effect(frame)?;
    let blur_input = snapshot.cast::<ID2D1Image>()?;
    unsafe {
        blur_effect.SetInput(0, &blur_input, true);
        blur_effect.SetValue(
            D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32,
            D2D1_PROPERTY_TYPE_FLOAT,
            &blur_sigma.to_ne_bytes(),
        )?;
        blur_effect.SetValue(
            D2D1_GAUSSIANBLUR_PROP_BORDER_MODE.0 as u32,
            D2D1_PROPERTY_TYPE_ENUM,
            &D2D1_BORDER_MODE_HARD.0.to_ne_bytes(),
        )?;
        let blur_output = blur_effect.GetOutput()?;
        frame
            .d2d_context
            .PushAxisAlignedClip(&to_d2d_rect(draw_rect), D2D1_ANTIALIAS_MODE_ALIASED);
        frame.d2d_context.DrawImage(
            &blur_output,
            Some(&Vector2 {
                X: capture_rect.left as f32,
                Y: capture_rect.top as f32,
            }),
            None,
            D2D1_INTERPOLATION_MODE_LINEAR,
            D2D1_COMPOSITE_MODE_SOURCE_OVER,
        );
        frame.d2d_context.PopAxisAlignedClip();
    }

    Ok(())
}

fn current_loader_blur_strength() -> f32 {
    f32::from_bits(LOADER_BLUR_STRENGTH_BITS.load(Ordering::Acquire)).clamp(0.0, 2.4)
}

fn draw_data_has_work(draw_data: &arcui_core::DrawData) -> bool {
    draw_data.lists.iter().any(|list| !list.commands.is_empty())
}

fn ensure_blur_snapshot(
    frame: &mut FrameState,
    width: u32,
    height: u32,
) -> windows::core::Result<ID2D1Bitmap1> {
    if frame.blur_snapshot_size != (width, height) || frame.blur_snapshot.is_none() {
        let bitmap_props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_R8G8B8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: Default::default(),
            colorContext: ManuallyDrop::new(None),
        };
        let snapshot = unsafe {
            frame
                .d2d_context
                .CreateBitmap(D2D_SIZE_U { width, height }, None, 0, &bitmap_props)
        }?;
        frame.blur_snapshot = Some(snapshot);
        frame.blur_snapshot_size = (width, height);
    }

    Ok(frame
        .blur_snapshot
        .as_ref()
        .expect("blur snapshot initialized")
        .clone())
}

fn ensure_blur_effect(frame: &mut FrameState) -> windows::core::Result<ID2D1Effect> {
    if frame.blur_effect.is_none() {
        frame.blur_effect =
            Some(unsafe { frame.d2d_context.CreateEffect(&CLSID_D2D1GaussianBlur) }?);
    }

    Ok(frame
        .blur_effect
        .as_ref()
        .expect("blur effect initialized")
        .clone())
}

fn clamp_rect_to_output(rect: Rect, width: f32, height: f32) -> Rect {
    Rect::from_min_max(
        Vec2::new(rect.min.x.clamp(0.0, width), rect.min.y.clamp(0.0, height)),
        Vec2::new(rect.max.x.clamp(0.0, width), rect.max.y.clamp(0.0, height)),
    )
}

fn expanded_capture_rect(rect: Rect, padding: f32, width: u32, height: u32) -> D2D_RECT_U {
    D2D_RECT_U {
        left: (rect.min.x - padding).floor().max(0.0) as u32,
        top: (rect.min.y - padding).floor().max(0.0) as u32,
        right: (rect.max.x + padding).ceil().min(width as f32) as u32,
        bottom: (rect.max.y + padding).ceil().min(height as f32) as u32,
    }
}

fn ensure_window_hook(swap_chain: &IDXGISwapChain3) -> windows::core::Result<()> {
    if WNDPROC_SUBCLASSED.load(Ordering::Acquire) {
        return Ok(());
    }

    // ChiyanMap first uses DXGI's OutputWindow and then falls back to locating
    // Minecraft's real input HWND. CreateSwapChainForCoreWindow can leave
    // DXGI_SWAP_CHAIN_DESC::OutputWindow null even though rendering succeeds.
    let hwnd = resolve_input_window(swap_chain);
    if hwnd.0.is_null() {
        return Ok(());
    }

    let previous = unsafe {
        SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            host_wnd_proc as *const () as usize as isize,
        )
    };
    if previous != 0 {
        ORIGINAL_WNDPROC.store(previous, Ordering::Release);
        HOST_HWND_RAW.store(hwnd.0 as isize, Ordering::Release);
        WNDPROC_SUBCLASSED.store(true, Ordering::Release);
    }

    Ok(())
}

fn resolve_input_window(swap_chain: &IDXGISwapChain3) -> HWND {
    if let Ok(output) = extract_output_window(swap_chain) {
        if !output.0.is_null() {
            return output;
        }
    }

    // Exact ChiyanMap-compatible class-name fallback, but do not accidentally
    // subclass another Minecraft process.
    let minecraft =
        unsafe { FindWindowW(windows::core::w!("Minecraft").as_ptr(), std::ptr::null()) };
    if window_belongs_to_current_process(minecraft) {
        return minecraft;
    }

    // GDK/Preview builds can expose a title rather than the classic class.
    for title in [
        windows::core::w!("Minecraft"),
        windows::core::w!("Minecraft Preview"),
        windows::core::w!("Minecraft for Windows"),
    ] {
        let candidate = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
        if window_belongs_to_current_process(candidate) {
            return candidate;
        }
    }

    // UWP builds often render through CreateSwapChainForCoreWindow, leaving
    // OutputWindow null. Their process-owned CoreWindow is normally a child of
    // ApplicationFrameHost's ApplicationFrameWindow. Search that child rather
    // than attempting to subclass the cross-process frame window.
    let root = HWND(std::ptr::null_mut());
    let mut frame_after = HWND(std::ptr::null_mut());
    loop {
        let frame = unsafe {
            FindWindowExW(
                root,
                frame_after,
                windows::core::w!("ApplicationFrameWindow").as_ptr(),
                std::ptr::null(),
            )
        };
        if frame.0.is_null() {
            break;
        }
        frame_after = frame;

        let mut child_after = HWND(std::ptr::null_mut());
        loop {
            let child = unsafe {
                FindWindowExW(
                    frame,
                    child_after,
                    windows::core::w!("Windows.UI.Core.CoreWindow").as_ptr(),
                    std::ptr::null(),
                )
            };
            if child.0.is_null() {
                break;
            }
            child_after = child;
            if window_belongs_to_current_process(child) {
                return child;
            }
        }
    }

    // Final safe fallback: only accept the foreground window when it belongs
    // to the injected Minecraft process.
    let foreground = unsafe { GetForegroundWindow() };
    if window_belongs_to_current_process(foreground) {
        return foreground;
    }

    HWND(std::ptr::null_mut())
}

fn window_belongs_to_current_process(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid != 0 && pid == unsafe { GetCurrentProcessId() }
}

fn create_frame_state(
    swap_chain: &IDXGISwapChain3,
    command_queue: &ID3D12CommandQueue,
) -> windows::core::Result<FrameState> {
    let device12: ID3D12Device = unsafe { try_out_ptr(|v| command_queue.GetDevice(v)) }?;
    let command_allocator =
        unsafe { device12.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT) }?;
    let command_list: ID3D12GraphicsCommandList = unsafe {
        device12.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &command_allocator, None)
    }?;
    unsafe {
        command_list.Close()?;
    }
    let fence = unsafe { device12.CreateFence(0, D3D12_FENCE_FLAG_NONE) }?;
    let fence_event = unsafe { CreateEventW(None, false, false, None)? };

    let d3d11_device = create_d3d11on12_device(&device12, command_queue)?;
    let d3d11_context = unsafe { d3d11_device.GetImmediateContext()? };
    let d3d11on12_device: ID3D11On12Device = d3d11_device.cast()?;

    let dxgi_device: IDXGIDevice = d3d11_device.cast()?;
    let d2d_factory: ID2D1Factory1 =
        unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None) }?;
    let d2d_device: ID2D1Device = unsafe { d2d_factory.CreateDevice(&dxgi_device) }?;
    let d2d_context: ID2D1DeviceContext =
        unsafe { d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE) }?;
    let dwrite_factory: IDWriteFactory =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }?;
    let text_format = unsafe {
        dwrite_factory.CreateTextFormat(
            w!("Microsoft YaHei UI"),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            16.0,
            w!("zh-CN"),
        )
    }?;
    unsafe {
        text_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
        text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        text_format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    }

    let initial_color = D2D1_COLOR_F {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
    let brush = unsafe { d2d_context.CreateSolidColorBrush(&initial_color, None) }?;

    let desc = unsafe { swap_chain.GetDesc1()? };
    let mut back_buffers = Vec::with_capacity(desc.BufferCount as usize);
    for index in 0..desc.BufferCount {
        let d3d12_resource: ID3D12Resource = unsafe { swap_chain.GetBuffer(index)? };

        let flags = D3D11_RESOURCE_FLAGS {
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let d3d11_resource: ID3D11Resource = unsafe {
            try_out_ptr(|v| {
                d3d11on12_device.CreateWrappedResource(
                    &d3d12_resource,
                    &flags,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    D3D12_RESOURCE_STATE_PRESENT,
                    v,
                )
            })
        }?;

        let surface: IDXGISurface = d3d11_resource.cast()?;
        let bitmap_props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_R8G8B8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: ManuallyDrop::new(None),
        };
        let d2d_target =
            unsafe { d2d_context.CreateBitmapFromDxgiSurface(&surface, Some(&bitmap_props)) }?;

        back_buffers.push(WrappedBackBuffer {
            d3d12_resource,
            d3d11_resource,
            d2d_target,
        });
    }

    Ok(FrameState {
        swap_chain: swap_chain.clone(),
        device12,
        command_queue: command_queue.clone(),
        command_allocator,
        command_list,
        fence,
        fence_value: 0,
        fence_event: fence_event.0 as isize,
        d3d11_context,
        d3d11on12_device,
        d2d_context,
        text_format,
        brush,
        blur_effect: None,
        blur_snapshot: None,
        blur_snapshot_size: (0, 0),
        back_buffers,
    })
}

fn create_d3d11on12_device(
    device12: &ID3D12Device,
    command_queue: &ID3D12CommandQueue,
) -> windows::core::Result<ID3D11Device> {
    let mut d3d11_device = None;
    let mut d3d11_context = None;
    let queues = [Some(command_queue.cast::<IUnknown>()?)];
    unsafe {
        D3D11On12CreateDevice(
            device12,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT.0 as u32,
            None,
            Some(&queues),
            0,
            Some(&mut d3d11_device),
            Some(&mut d3d11_context),
            None,
        )?;
    }
    d3d11_device
        .ok_or_else(|| windows::core::Error::from(windows::core::HRESULT(0x80004005u32 as i32)))
}

fn wait_for_overlay_queue(frame: &FrameState) -> windows::core::Result<()> {
    unsafe {
        if frame.fence.GetCompletedValue() < frame.fence_value {
            frame.fence.SetEventOnCompletion(
                frame.fence_value,
                windows::Win32::Foundation::HANDLE(frame.fence_event as *mut c_void),
            )?;
            WaitForSingleObject(
                windows::Win32::Foundation::HANDLE(frame.fence_event as *mut c_void),
                u32::MAX,
            );
        }
    }

    Ok(())
}

fn teardown_frame_state() {
    if let Some(slot) = FRAME_STATE.get() {
        let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut frame) = guard.take() {
            release_all_resources(&mut frame);
        }
    }
}

fn release_all_resources(frame: &mut FrameState) {
    let _ = wait_for_overlay_queue(frame);

    unsafe {
        let _ = frame.d2d_context.SetTarget(None::<&ID2D1Image>);
        frame.d3d11_context.ClearState();
        frame.d3d11_context.Flush();
    }

    frame.blur_effect = None;
    frame.blur_snapshot = None;
    frame.blur_snapshot_size = (0, 0);
    frame.back_buffers.clear();

    if frame.fence_event != 0 {
        unsafe {
            let _ = CloseHandle(windows::Win32::Foundation::HANDLE(
                frame.fence_event as *mut c_void,
            ));
        }
        frame.fence_event = 0;
    }
}

fn begin_resize_transition(width: u32, height: u32) {
    teardown_frame_state();
    RESIZE_PENDING.store(true, Ordering::SeqCst);
    RESIZE_SKIP_FRAMES.store(RESIZE_SKIP_FRAME_COUNT, Ordering::SeqCst);
    RESIZE_LAST_SIZE.store(pack_size(width, height), Ordering::SeqCst);
    RESIZE_STABLE_FRAMES.store(0, Ordering::SeqCst);
}

fn should_render_this_frame(swap_chain: &IDXGISwapChain3) -> bool {
    let skip = RESIZE_SKIP_FRAMES.load(Ordering::Acquire);
    if skip > 0 {
        RESIZE_SKIP_FRAMES.store(skip - 1, Ordering::Release);
        return false;
    }

    let Ok(desc) = (unsafe { swap_chain.GetDesc1() }) else {
        RESIZE_PENDING.store(true, Ordering::Release);
        RESIZE_STABLE_FRAMES.store(0, Ordering::Release);
        return false;
    };

    let width = desc.Width;
    let height = desc.Height;
    if width == 0 || height == 0 {
        RESIZE_PENDING.store(true, Ordering::Release);
        RESIZE_STABLE_FRAMES.store(0, Ordering::Release);
        return false;
    }

    let packed = pack_size(width, height);
    let previous = RESIZE_LAST_SIZE.swap(packed, Ordering::AcqRel);
    if previous != 0 && previous != packed {
        RESIZE_PENDING.store(true, Ordering::Release);
        RESIZE_SKIP_FRAMES.store(RESIZE_SKIP_FRAME_COUNT, Ordering::Release);
        RESIZE_STABLE_FRAMES.store(1, Ordering::Release);
        teardown_frame_state();
        return false;
    }

    if RESIZE_PENDING.load(Ordering::Acquire) {
        let stable = if previous == packed {
            RESIZE_STABLE_FRAMES.fetch_add(1, Ordering::AcqRel) + 1
        } else {
            RESIZE_STABLE_FRAMES.store(1, Ordering::Release);
            1
        };
        if stable < RESIZE_REQUIRED_STABLE_FRAMES {
            return false;
        }
        RESIZE_PENDING.store(false, Ordering::Release);
    }

    true
}

fn frame_state_matches_swap_chain(frame: &FrameState, swap_chain: &IDXGISwapChain3) -> bool {
    if frame.swap_chain.as_raw() != swap_chain.as_raw() {
        return false;
    }

    let Ok(desc) = (unsafe { swap_chain.GetDesc1() }) else {
        return false;
    };
    if desc.Width == 0 || desc.Height == 0 {
        return false;
    }

    frame.back_buffers.len() == desc.BufferCount as usize
}

fn pack_size(width: u32, height: u32) -> u64 {
    ((width as u64) << 32) | height as u64
}

fn create_transition_barrier(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

fn schedule_cursor_warp(delay: Duration) {
    let generation = CURSOR_WARP_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;

    thread::spawn(move || {
        thread::sleep(delay);

        if !CAPTURE_INPUT.load(Ordering::Acquire)
            || CURSOR_WARP_GENERATION.load(Ordering::Acquire) != generation
        {
            return;
        }

        let Some(hwnd) = current_host_hwnd() else {
            return;
        };

        // The user may switch applications during the 100 ms hand-off. Never
        // move the desktop cursor unless Minecraft is still the foreground
        // window owned by this process.
        let foreground = unsafe { GetForegroundWindow() };
        if foreground != hwnd && !window_belongs_to_current_process(foreground) {
            return;
        }

        let Some(region) = current_capture_region() else {
            return;
        };

        let client_x = ((region.min.x + region.max.x) * 0.5).round() as i32;
        let client_y = ((region.min.y + region.max.y) * 0.5).round() as i32;
        let mut screen_point = POINT {
            x: client_x,
            y: client_y,
        };

        if !unsafe { ClientToScreen(hwnd, &mut screen_point) }.as_bool() {
            return;
        }

        let _ = unsafe { ClipCursor(None) };
        CURSOR_WARP_BYPASS.with(|bypass| {
            bypass.set(true);
            let _ = unsafe { SetCursorPos(screen_point.x, screen_point.y) };
            bypass.set(false);
        });

        // Force ArcUI's platform state to observe the new client coordinate in
        // the same input chain. host_wnd_proc dispatches this message to ArcUI
        // first and consumes it before Minecraft sees it.
        let packed_x = (client_x as i16 as u16) as u32;
        let packed_y = (client_y as i16 as u16) as u32;
        let packed = packed_x | (packed_y << 16);
        let _ =
            unsafe { PostMessageW(Some(hwnd), WM_MOUSEMOVE, WPARAM(0), LPARAM(packed as isize)) };
    });
}

fn current_host_hwnd() -> Option<HWND> {
    let raw = HOST_HWND_RAW.load(Ordering::Acquire);
    if raw == 0 {
        None
    } else {
        Some(HWND(raw as *mut _))
    }
}

fn current_capture_region() -> Option<Rect> {
    let guard = capture_region().lock().unwrap_or_else(|e| e.into_inner());
    *guard
}

fn capture_region() -> &'static Mutex<Option<Rect>> {
    CAPTURE_REGION.get_or_init(|| Mutex::new(None))
}

fn release_host_input_state() {
    let Some(hwnd) = current_host_hwnd() else {
        return;
    };

    const KEY_RELEASES: &[u32] = &[
        0x57, 0x41, 0x53, 0x44, 0x20, 0x10, 0x11, 0x12, 0x09, 0x45, 0x51, 0x46, 0x52, 0x43, 0x58,
        0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x30, 0x25, 0x26, 0x27, 0x28,
    ];

    for &vk in KEY_RELEASES {
        let message = if vk == 0x12 { WM_SYSKEYUP } else { WM_KEYUP };
        let _ = unsafe { PostMessageW(Some(hwnd), message, WPARAM(vk as usize), LPARAM(0)) };
    }

    let _ = unsafe { PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(0)) };
    let _ = unsafe { PostMessageW(Some(hwnd), WM_RBUTTONUP, WPARAM(0), LPARAM(0)) };
    let _ = unsafe { PostMessageW(Some(hwnd), WM_MBUTTONUP, WPARAM(0), LPARAM(0)) };
    let _ = unsafe { PostMessageW(Some(hwnd), WM_XBUTTONUP, WPARAM(0), LPARAM(0)) };
}

fn loader_blur_state() -> &'static Mutex<Option<LoaderBlurState>> {
    LOADER_BLUR_STATE.get_or_init(|| Mutex::new(None))
}

fn unpack_color(color: u32) -> D2D1_COLOR_F {
    let r = (color & 0xFF) as f32 / 255.0;
    let g = ((color >> 8) & 0xFF) as f32 / 255.0;
    let b = ((color >> 16) & 0xFF) as f32 / 255.0;
    let a = ((color >> 24) & 0xFF) as f32 / 255.0;
    D2D1_COLOR_F { r, g, b, a }
}

fn to_d2d_rect(rect: arcui_core::Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: rect.min.x,
        top: rect.min.y,
        right: rect.max.x,
        bottom: rect.max.y,
    }
}

fn command_queue_matches_swap_chain(
    swap_chain: &IDXGISwapChain3,
    command_queue: &ID3D12CommandQueue,
) -> bool {
    unsafe {
        let swap_chain_ptr = swap_chain.as_raw() as *const *const c_void;
        let queue_ptr = command_queue.as_raw();
        for i in 0..512usize {
            if swap_chain_ptr.add(i).read() == queue_ptr {
                return true;
            }
        }
    }
    false
}

fn extract_output_window(swap_chain: &IDXGISwapChain3) -> windows::core::Result<HWND> {
    let base_swap_chain: IDXGISwapChain = swap_chain.cast()?;
    Ok(unsafe { base_swap_chain.GetDesc()?.OutputWindow })
}

fn get_target_addrs() -> Result<HookTargets, String> {
    let dummy = dummy_hwnd::create()?;
    let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) }
        .map_err(|e| format!("{e:?}"))?;
    let adapter = unsafe { factory.EnumAdapters(0) }.map_err(|e| format!("{e:?}"))?;
    let device: ID3D12Device =
        unsafe { try_out_ptr(|v| D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, v)) }
            .map_err(|e| format!("{e:?}"))?;
    let command_queue: ID3D12CommandQueue = unsafe {
        device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
            Priority: 0,
            Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        })
    }
    .map_err(|e| format!("{e:?}"))?;

    let swap_chain: IDXGISwapChain3 = unsafe {
        factory.CreateSwapChainForHwnd(
            &command_queue,
            dummy,
            &DXGI_SWAP_CHAIN_DESC1 {
                Width: 640,
                Height: 480,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                Stereo: BOOL(0),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_NONE,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: Default::default(),
                Flags: DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH.0 as u32,
            },
            None,
            None,
        )
    }
    .map_err(|e| format!("{e:?}"))?
    .cast()
    .map_err(|e| format!("{e:?}"))?;

    let base_swap_chain: IDXGISwapChain = swap_chain.cast().map_err(|e| format!("{e:?}"))?;
    let present = unsafe { mem::transmute(base_swap_chain.vtable().Present) };
    let execute_command_lists =
        unsafe { mem::transmute(command_queue.vtable().ExecuteCommandLists) };
    let resize_buffers = unsafe { mem::transmute(base_swap_chain.vtable().ResizeBuffers) };
    let swap_chain_vtable = unsafe { *(swap_chain.as_raw() as *const *const *const c_void) };
    let resize_buffers1 = unsafe { mem::transmute(*swap_chain_vtable.add(39)) };

    Ok(HookTargets {
        present,
        execute_command_lists,
        resize_buffers,
        resize_buffers1,
    })
}

unsafe fn try_out_ptr<T, F>(mut f: F) -> windows::core::Result<T>
where
    F: FnMut(&mut Option<T>) -> windows::core::Result<()>,
{
    let mut out = None;
    f(&mut out)?;
    Ok(out.expect("out pointer"))
}
