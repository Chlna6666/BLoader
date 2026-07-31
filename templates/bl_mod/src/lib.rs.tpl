use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use bl_sdk::{
    bl_export_mod, BlEventCallback, Host, BL_EVENT_KEY, BL_EVENT_TICK,
};

static SHOW_WINDOW: AtomicBool = AtomicBool::new(true);
static FRAME_INDEX: AtomicU64 = AtomicU64::new(0);
static RESOURCE_RELOADS: AtomicU64 = AtomicU64::new(0);

unsafe extern "system" fn on_event(event_id: u32, payload: *const c_void, _user_data: *mut c_void) {
    match event_id {
        BL_EVENT_TICK => {
            let tick = unsafe { &*(payload as *const bl_sdk::BlTickEvent) };
            FRAME_INDEX.store(tick.frame_index, Ordering::Relaxed);
        }
        BL_EVENT_KEY => {
            let key = unsafe { &*(payload as *const bl_sdk::BlKeyEvent) };
            if key.virtual_key == 0x79 && key.is_down != 0 {
                let current = SHOW_WINDOW.load(Ordering::Relaxed);
                SHOW_WINDOW.store(!current, Ordering::Relaxed);
            }
        }
        _ => {}
    }
}

unsafe extern "system" fn draw_ui(_user_data: *mut c_void) {
    let host_ptr = BL_HOST.load(Ordering::Relaxed);
    if host_ptr.is_null() || !SHOW_WINDOW.load(Ordering::Relaxed) {
        return;
    }
    let host = Host::from_raw(host_ptr);

    let mut open = true;
    if host.ui_begin_window("{{mod_name}}", Some(&mut open), 0) {
        host.ui_text("BL template mod is running.");
        host.ui_separator();
        host.ui_text(&format!("Frame: {}", FRAME_INDEX.load(Ordering::Relaxed)));
        host.ui_text(&format!("Resource reloads: {}", RESOURCE_RELOADS.load(Ordering::Relaxed)));

        if host.ui_button("Toggle Window (F10)") {
            SHOW_WINDOW.store(false, Ordering::Relaxed);
        }
        host.ui_end_window();
    }
    SHOW_WINDOW.store(open, Ordering::Relaxed);
}

static BL_HOST: AtomicPtr<bl_sdk::BlHostApiV1> = AtomicPtr::new(std::ptr::null_mut());

unsafe extern "system" fn on_resource_reload(_reason: u32, _user_data: *mut c_void) {
    RESOURCE_RELOADS.fetch_add(1, Ordering::Relaxed);
}

fn on_load(host: &Host) -> i32 {
    BL_HOST.store(host.raw() as *const _ as *mut _, Ordering::Relaxed);
    host.info("{{mod_name}} loaded");
    host.info("Most runtime gameplay events become available only after a world/map is loaded.");

    host.register_event("{{mod_id}}.events", on_event as BlEventCallback, std::ptr::null_mut());
    host.register_ui_panel("{{mod_id}}.ui", draw_ui, std::ptr::null_mut());
    host.register_resource("{{mod_id}}.resources", on_resource_reload, std::ptr::null_mut());
    0
}

fn on_unload() {}

bl_export_mod!(
    on_load: on_load,
    on_unload: on_unload
);
