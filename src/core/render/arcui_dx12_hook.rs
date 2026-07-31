use std::ffi::c_void;
use std::mem::{self, ManuallyDrop};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use minhook::MinHook;
use windows::core::{BOOL, Interface};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D12::{
    D3D12CreateDevice, ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue,
    ID3D12Device, ID3D12Fence, ID3D12GraphicsCommandList, ID3D12Resource,
    D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_QUEUE_DESC, D3D12_COMMAND_QUEUE_FLAG_NONE,
    D3D12_FENCE_FLAG_NONE, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
    D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, D3D12_RESOURCE_BARRIER_FLAG_NONE,
    D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_STATE_PRESENT,
    D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATES, D3D12_RESOURCE_TRANSITION_BARRIER,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIFactory2, IDXGISwapChain, IDXGISwapChain3, DXGI_CREATE_FACTORY_FLAGS,
    DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::bl_dummy_hwnd;
use crate::runtime::foundation::logging;

type PresentFn =
    unsafe extern "system" fn(this: IDXGISwapChain3, sync_interval: u32, flags: u32) -> windows::core::HRESULT;
type ExecuteCommandListsFn = unsafe extern "system" fn(
    this: ID3D12CommandQueue,
    num_command_lists: u32,
    command_lists: *mut ID3D12CommandList,
);

struct HookTargets {
    present: PresentFn,
    execute_command_lists: ExecuteCommandListsFn,
}

struct InitContext {
    swap_chain: Option<IDXGISwapChain3>,
    command_queue: Option<ID3D12CommandQueue>,
}

struct FrameState {
    device: ID3D12Device,
    command_queue: ID3D12CommandQueue,
    command_allocator: ID3D12CommandAllocator,
    command_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_value: u64,
    fence_event: isize,
}

static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
static ORIGINAL_PRESENT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_EXECUTE_COMMAND_LISTS: AtomicUsize = AtomicUsize::new(0);
static INIT_CONTEXT: OnceLock<Mutex<InitContext>> = OnceLock::new();
static FRAME_STATE: OnceLock<Mutex<Option<FrameState>>> = OnceLock::new();
static LAST_SIZE: AtomicU64 = AtomicU64::new(0);

pub fn install() -> bool {
    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return true;
    }

    let Ok(targets) = get_target_addrs() else {
        HOOK_INSTALLED.store(false, Ordering::SeqCst);
        logging::warn_message("arcui dx12 hook: failed to resolve DX12 targets");
        return false;
    };

    let present = match unsafe {
        MinHook::create_hook(targets.present as *mut c_void, detour_present as *mut c_void)
    } {
        Ok(original) => original,
        Err(error) => {
            HOOK_INSTALLED.store(false, Ordering::SeqCst);
            logging::warn_message(&format!("arcui dx12 hook: create Present hook failed: {error:?}"));
            return false;
        }
    };
    ORIGINAL_PRESENT.store(present as usize, Ordering::SeqCst);

    let execute = match unsafe {
        MinHook::create_hook(
            targets.execute_command_lists as *mut c_void,
            detour_execute_command_lists as *mut c_void,
        )
    } {
        Ok(original) => original,
        Err(error) => {
            HOOK_INSTALLED.store(false, Ordering::SeqCst);
            logging::warn_message(&format!(
                "arcui dx12 hook: create ExecuteCommandLists hook failed: {error:?}"
            ));
            return false;
        }
    };
    ORIGINAL_EXECUTE_COMMAND_LISTS.store(execute as usize, Ordering::SeqCst);

    if let Err(error) = unsafe { MinHook::enable_all_hooks() } {
        HOOK_INSTALLED.store(false, Ordering::SeqCst);
        logging::warn_message(&format!("arcui dx12 hook: enable hooks failed: {error:?}"));
        return false;
    }

    let _ = INIT_CONTEXT.set(Mutex::new(InitContext {
        swap_chain: None,
        command_queue: None,
    }));
    let _ = FRAME_STATE.set(Mutex::new(None));
    logging::info_message("arcui dx12 hook installed.");
    true
}

unsafe extern "system" fn detour_present(
    swap_chain: IDXGISwapChain3,
    sync_interval: u32,
    flags: u32,
) -> windows::core::HRESULT {
    if let Some(slot) = INIT_CONTEXT.get() {
        if let Ok(mut guard) = slot.lock() {
            if guard.swap_chain.is_none() {
                guard.swap_chain = Some(swap_chain.clone());
            }
        }
    }

    let _ = render_arcui(&swap_chain);

    let original: PresentFn = mem::transmute(ORIGINAL_PRESENT.load(Ordering::SeqCst));
    original(swap_chain, sync_interval, flags)
}

unsafe extern "system" fn detour_execute_command_lists(
    command_queue: ID3D12CommandQueue,
    num_command_lists: u32,
    command_lists: *mut ID3D12CommandList,
) {
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

    let original: ExecuteCommandListsFn =
        mem::transmute(ORIGINAL_EXECUTE_COMMAND_LISTS.load(Ordering::SeqCst));
    original(command_queue, num_command_lists, command_lists);
}

fn render_arcui(swap_chain: &IDXGISwapChain3) -> windows::core::Result<()> {
    let frame_slot = FRAME_STATE.get().expect("frame state initialized");
    let mut frame_guard = frame_slot.lock().unwrap_or_else(|e| e.into_inner());

    if frame_guard.is_none() {
        let init_slot = INIT_CONTEXT.get().expect("init context initialized");
        let init_guard = init_slot.lock().unwrap_or_else(|e| e.into_inner());
        let Some(command_queue) = init_guard.command_queue.clone() else {
            return Ok(());
        };
        let device: ID3D12Device =
            unsafe { try_out_ptr(|v| command_queue.GetDevice(v)) }?;
        let command_allocator: ID3D12CommandAllocator =
            unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)? };
        let command_list: ID3D12GraphicsCommandList = unsafe {
            device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &command_allocator, None)
        }?;
        unsafe { command_list.Close()? };
        let fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE)? };
        let fence_event = unsafe { CreateEventW(None, false, false, None)? };

        *frame_guard = Some(FrameState {
            device,
            command_queue,
            command_allocator,
            command_list,
            fence,
            fence_value: 0,
            fence_event: fence_event.0 as isize,
        });
    }

    let Some(frame) = frame_guard.as_mut() else {
        return Ok(());
    };

    let render_target: ID3D12Resource = unsafe { swap_chain.GetBuffer(swap_chain.GetCurrentBackBufferIndex())? };
    let desc = unsafe { render_target.GetDesc() };
    let width = desc.Width as u32;
    let height = desc.Height;
    if width == 0 || height == 0 {
        return Ok(());
    }

    let packed = ((width as u64) << 32) | height as u64;
    LAST_SIZE.store(packed, Ordering::SeqCst);

    wait_for_gpu(frame)?;

    unsafe {
        frame.command_allocator.Reset()?;
        frame.command_list.Reset(&frame.command_allocator, None)?;
    }

    let to_rtv = create_barrier(&render_target, D3D12_RESOURCE_STATE_PRESENT, D3D12_RESOURCE_STATE_RENDER_TARGET);
    let to_present =
        create_barrier(&render_target, D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_PRESENT);

    unsafe {
        frame.command_list.ResourceBarrier(&[to_rtv]);
    }

    unsafe {
        crate::core::arcui_dx12::render_demo(
            frame.device.clone().into_raw() as *mut c_void,
            frame.command_list.clone().into_raw() as *mut c_void,
            render_target.clone().into_raw() as *mut c_void,
            width,
            height,
        );
    }

    unsafe {
        frame.command_list.ResourceBarrier(&[to_present]);
        frame.command_list.Close()?;
        frame.command_queue
            .ExecuteCommandLists(&[Some(frame.command_list.cast()?)]);
        frame.fence_value += 1;
        frame.command_queue.Signal(&frame.fence, frame.fence_value)?;
    }

    Ok(())
}

fn wait_for_gpu(frame: &FrameState) -> windows::core::Result<()> {
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

fn command_queue_matches_swap_chain(
    swap_chain: &IDXGISwapChain3,
    command_queue: &ID3D12CommandQueue,
) -> bool {
    unsafe {
        let swap_chain_ptr = swap_chain.as_raw() as *const *const c_void;
        let queue_ptr = command_queue.as_raw();
        for i in 0..512usize {
            let current = swap_chain_ptr.add(i).read();
            if current == queue_ptr {
                return true;
            }
        }
    }
    false
}

fn get_target_addrs() -> windows::core::Result<HookTargets> {
    let dummy = bl_dummy_hwnd::dummy_hwnd();
    let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) }?;
    let adapter = unsafe { factory.EnumAdapters(0) }?;
    let device: ID3D12Device =
        unsafe { try_out_ptr(|v| D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, v)) }?;
    let command_queue: ID3D12CommandQueue = unsafe {
        device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
            Priority: 0,
            Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        })
    }?;

    let swap_chain: IDXGISwapChain3 = unsafe {
        factory.CreateSwapChainForHwnd(
            &command_queue,
            dummy,
            &DXGI_SWAP_CHAIN_DESC1 {
                Width: 640,
                Height: 480,
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

    let base_swap_chain: IDXGISwapChain = swap_chain.cast()?;
    let present = unsafe { mem::transmute(base_swap_chain.vtable().Present) };
    let execute_command_lists = unsafe { mem::transmute(command_queue.vtable().ExecuteCommandLists) };

    Ok(HookTargets {
        present,
        execute_command_lists,
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

fn create_barrier(
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
