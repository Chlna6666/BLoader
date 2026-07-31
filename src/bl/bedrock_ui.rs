use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Default)]
struct BedrockUiState {
    resource_pack_dir: Option<PathBuf>,
    registered_screens: Vec<String>,
    requested_screen: Option<String>,
    delivered_screen: Option<String>,
    tessellator_hook_ready: bool,
}

static STATE: OnceLock<Mutex<BedrockUiState>> = OnceLock::new();

fn state() -> &'static Mutex<BedrockUiState> {
    STATE.get_or_init(|| Mutex::new(BedrockUiState::default()))
}

fn lock_state() -> std::sync::MutexGuard<'static, BedrockUiState> {
    state().lock().unwrap_or_else(|error| error.into_inner())
}

pub fn resource_pack_dir() -> Option<PathBuf> {
    lock_state().resource_pack_dir.clone()
}

pub fn request_screen(screen_id: &str) {
    let mut state = lock_state();
    state.requested_screen = Some(screen_id.to_string());
}

pub fn requested_screen() -> Option<String> {
    lock_state().requested_screen.clone()
}

pub fn delivered_screen() -> Option<String> {
    lock_state().delivered_screen.clone()
}

pub fn registered_screens() -> Vec<String> {
    lock_state().registered_screens.clone()
}

pub fn tessellator_hook_ready() -> bool {
    lock_state().tessellator_hook_ready
}

pub fn status_summary() -> String {
    let state = lock_state();
    format!(
        "resource_pack={} registered_screens={} requested_screen={} delivered_screen={} tessellator_ready={}",
        state
            .resource_pack_dir
            .as_ref()
            .map(|value| value.display().to_string())
            .unwrap_or_else(|| "<unavailable>".to_string()),
        state.registered_screens.len(),
        state
            .requested_screen
            .clone()
            .unwrap_or_else(|| "<none>".to_string()),
        state
            .delivered_screen
            .clone()
            .unwrap_or_else(|| "<none>".to_string()),
        state.tessellator_hook_ready
    )
}
