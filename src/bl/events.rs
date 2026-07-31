#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use once_cell::sync::Lazy;

use crate::bl::abi::{
    BL_EVENT_BLOCK_ACTION, BL_EVENT_CHAT, BL_EVENT_CLIENT_JOIN_LEVEL, BL_EVENT_CREATED_LEVEL,
    BL_EVENT_KEY, BL_EVENT_LOCAL_PLAYER_BOUND, BL_EVENT_PLAYER_ACTION, BL_EVENT_RENDER_3D,
    BL_EVENT_RENDER_FRAME, BL_EVENT_SET_LOCAL_PLAYER_AS_INIT, BL_EVENT_START_GAME_PACKET,
    BL_EVENT_TICK, BL_EVENT_WORLD_ENTER, BlBlockActionEvent, BlChatEvent, BlClientJoinLevelEvent,
    BlCreatedLevelEvent, BlKeyEvent, BlLocalPlayerBoundEvent, BlPlayerActionEvent, BlRender3DEvent,
    BlSetLocalPlayerAsInitEvent, BlStartGamePacketEvent, BlStringView, BlTickEvent,
    BlWorldEnterEvent,
};

static FRAME_INDEX: AtomicU64 = AtomicU64::new(0);
static LAST_FRAME_TIME: Lazy<std::sync::Mutex<Instant>> =
    Lazy::new(|| std::sync::Mutex::new(Instant::now()));
static FIRST_FRAME_TIME: Lazy<Instant> = Lazy::new(Instant::now);
static WORLD_ENTER_ONCE: Lazy<std::sync::Mutex<bool>> = Lazy::new(|| std::sync::Mutex::new(false));

pub fn emit_frame_tick() {
    crate::bl::adapter::refresh_runtime_world_from_level_scan();

    let now = Instant::now();
    let delta = {
        let mut last = LAST_FRAME_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let d = now.duration_since(*last).as_secs_f32();
        *last = now;
        d
    };
    let frame_index = FRAME_INDEX.fetch_add(1, Ordering::Relaxed) + 1;
    let payload = BlTickEvent {
        frame_index,
        delta_seconds: delta,
        total_seconds: now.duration_since(*FIRST_FRAME_TIME).as_secs_f64(),
    };
    crate::bl::host::dispatch_event(BL_EVENT_TICK, &payload as *const _ as *const _);
    crate::bl::host::dispatch_event(BL_EVENT_RENDER_FRAME, std::ptr::null());
}

pub fn emit_key_event(virtual_key: u32, is_down: bool, is_repeat: bool) {
    emit_key_event_with_modifiers(virtual_key, is_down, is_repeat, false, false, false);
}

pub fn emit_key_event_with_modifiers(
    virtual_key: u32,
    is_down: bool,
    is_repeat: bool,
    alt: bool,
    ctrl: bool,
    shift: bool,
) {
    if is_down && !is_repeat && virtual_key == 0x1B {
        crate::bl::adapter::notify_escape_pressed();
    }

    let payload = BlKeyEvent {
        virtual_key,
        is_down: if is_down { 1 } else { 0 },
        is_repeat: if is_repeat { 1 } else { 0 },
        alt: if alt { 1 } else { 0 },
        ctrl: if ctrl { 1 } else { 0 },
        shift: if shift { 1 } else { 0 },
        reserved: [0; 2],
    };
    crate::bl::host::dispatch_event(BL_EVENT_KEY, &payload as *const _ as *const _);
}

pub fn emit_world_enter(world_name: &str, source: &str) {
    let mut once = WORLD_ENTER_ONCE.lock().unwrap_or_else(|e| e.into_inner());
    *once = true;

    let world_name_owned = world_name.to_string();
    let source_owned = source.to_string();
    let payload = BlWorldEnterEvent {
        world_name: borrowed_view(&world_name_owned),
        source: borrowed_view(&source_owned),
    };
    crate::bl::host::dispatch_event(BL_EVENT_WORLD_ENTER, &payload as *const _ as *const _);
}

pub fn reset_world_state() {
    crate::bl::adapter::clear_world_state();
    crate::bl::client_state::clear();
    let mut once = WORLD_ENTER_ONCE.lock().unwrap_or_else(|e| e.into_inner());
    *once = false;
}

pub fn emit_chat(author: &str, message: &str, channel: &str) {
    let author_owned = author.to_string();
    let message_owned = message.to_string();
    let channel_owned = channel.to_string();
    let payload = BlChatEvent {
        author: borrowed_view(&author_owned),
        message: borrowed_view(&message_owned),
        channel: borrowed_view(&channel_owned),
    };
    crate::bl::host::dispatch_event(BL_EVENT_CHAT, &payload as *const _ as *const _);
}

pub fn emit_created_level(
    client_instance: usize,
    level: usize,
    route: &str,
    world_name: Option<&str>,
) {
    crate::bl::client_state::record_created_level(client_instance, level);
    let route_owned = route.to_string();
    let world_name_owned = world_name.unwrap_or_default().to_string();
    let payload = BlCreatedLevelEvent {
        client_instance,
        level,
        route: borrowed_view(&route_owned),
        world_name: borrowed_view(&world_name_owned),
    };
    crate::bl::host::dispatch_event(BL_EVENT_CREATED_LEVEL, &payload as *const _ as *const _);
}

pub fn emit_start_game_packet(this_ptr: usize, arg1: usize, arg2: usize) {
    let payload = BlStartGamePacketEvent {
        this_ptr,
        arg1,
        arg2,
    };
    crate::bl::host::dispatch_event(BL_EVENT_START_GAME_PACKET, &payload as *const _ as *const _);
}

pub fn emit_set_local_player_as_init(this_ptr: usize, arg1: usize, arg2: usize) {
    let payload = BlSetLocalPlayerAsInitEvent {
        this_ptr,
        arg1,
        arg2,
    };
    crate::bl::host::dispatch_event(
        BL_EVENT_SET_LOCAL_PLAYER_AS_INIT,
        &payload as *const _ as *const _,
    );
}

pub fn emit_local_player_bound(player_ptr: usize, client_instance: usize, route: &str) {
    crate::bl::client_state::record_local_player_bound(player_ptr, client_instance);
    let route_owned = route.to_string();
    let payload = BlLocalPlayerBoundEvent {
        player_ptr,
        client_instance,
        route: borrowed_view(&route_owned),
    };
    crate::bl::host::dispatch_event(
        BL_EVENT_LOCAL_PLAYER_BOUND,
        &payload as *const _ as *const _,
    );
}

pub fn emit_client_join_level(player_ptr: usize, client_instance: usize, route: &str) {
    crate::bl::client_state::record_client_join_level(player_ptr, client_instance);
    let route_owned = route.to_string();
    let payload = BlClientJoinLevelEvent {
        player_ptr,
        client_instance,
        route: borrowed_view(&route_owned),
    };
    crate::bl::host::dispatch_event(BL_EVENT_CLIENT_JOIN_LEVEL, &payload as *const _ as *const _);
}

pub fn emit_player_action(
    this_ptr: usize,
    packet_ptr: usize,
    arg1: usize,
    arg2: usize,
    action_code: u32,
    block_x: i32,
    block_y: i32,
    block_z: i32,
    face: i32,
    action_name: &str,
    status: &str,
) {
    let action_name_owned = action_name.to_string();
    let status_owned = status.to_string();
    let payload = BlPlayerActionEvent {
        this_ptr,
        packet_ptr,
        arg1,
        arg2,
        action_code,
        block_x,
        block_y,
        block_z,
        face,
        action_name: borrowed_view(&action_name_owned),
        status: borrowed_view(&status_owned),
    };
    crate::bl::host::dispatch_event(BL_EVENT_PLAYER_ACTION, &payload as *const _ as *const _);
}

pub fn emit_block_action(
    action_code: u32,
    block_x: i32,
    block_y: i32,
    block_z: i32,
    face: i32,
    action_name: &str,
) {
    let action_name_owned = action_name.to_string();
    let payload = BlBlockActionEvent {
        player_action: action_code,
        block_x,
        block_y,
        block_z,
        face,
        action_name: borrowed_view(&action_name_owned),
    };
    crate::bl::host::dispatch_event(BL_EVENT_BLOCK_ACTION, &payload as *const _ as *const _);
}

pub fn emit_render3d(level_render: usize, screen_context: usize) {
    let payload = BlRender3DEvent {
        level_render,
        screen_context,
    };
    crate::bl::host::dispatch_event(BL_EVENT_RENDER_3D, &payload as *const _ as *const _);
}

fn borrowed_view(text: &str) -> BlStringView {
    BlStringView {
        ptr: text.as_ptr() as *const i8,
        len: text.len(),
    }
}
