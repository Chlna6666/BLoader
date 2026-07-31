use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClientSnapshot {
    pub client_instance: usize,
    pub local_player: usize,
    pub level: usize,
}

static CLIENT_INSTANCE: AtomicUsize = AtomicUsize::new(0);
static LOCAL_PLAYER: AtomicUsize = AtomicUsize::new(0);
static LEVEL: AtomicUsize = AtomicUsize::new(0);

pub fn record_created_level(client_instance: usize, level: usize) {
    store_if_nonzero(&CLIENT_INSTANCE, client_instance);
    store_if_nonzero(&LEVEL, level);
}

pub fn record_local_player_bound(player_ptr: usize, client_instance: usize) {
    store_if_nonzero(&CLIENT_INSTANCE, client_instance);
    store_if_nonzero(&LOCAL_PLAYER, player_ptr);
}

pub fn record_client_join_level(player_ptr: usize, client_instance: usize) {
    record_local_player_bound(player_ptr, client_instance);
}

pub fn clear() {
    CLIENT_INSTANCE.store(0, Ordering::Release);
    LOCAL_PLAYER.store(0, Ordering::Release);
    LEVEL.store(0, Ordering::Release);
}

pub fn snapshot() -> ClientSnapshot {
    ClientSnapshot {
        client_instance: CLIENT_INSTANCE.load(Ordering::Acquire),
        local_player: LOCAL_PLAYER.load(Ordering::Acquire),
        level: LEVEL.load(Ordering::Acquire),
    }
}

pub fn runtime_value(key: &str) -> String {
    let state = snapshot();
    match key {
        "client.instance" => address_text(state.client_instance),
        "client.local_player" => address_text(state.local_player),
        "client.level" => address_text(state.level),
        "client.ready" | "client_instance.ready" => (state.client_instance != 0).to_string(),
        "client.local_player_ready" => (state.local_player != 0).to_string(),
        "client.status" => {
            if state.local_player != 0 {
                "local_player_ready".to_string()
            } else if state.client_instance != 0 {
                "client_instance_ready".to_string()
            } else {
                "unavailable".to_string()
            }
        }
        _ => String::new(),
    }
}

fn store_if_nonzero(target: &AtomicUsize, value: usize) {
    if value != 0 {
        target.store(value, Ordering::Release);
    }
}

fn address_text(address: usize) -> String {
    if address == 0 {
        String::new()
    } else {
        format!("0x{address:X}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_state_only_becomes_ready_after_a_binding_event() {
        clear();
        assert_eq!(runtime_value("client.status"), "unavailable");

        record_created_level(0x1000, 0x2000);
        assert_eq!(runtime_value("client.ready"), "true");
        assert_eq!(runtime_value("client.local_player_ready"), "false");
        assert_eq!(runtime_value("client.level"), "0x2000");

        record_local_player_bound(0x3000, 0x1000);
        assert_eq!(runtime_value("client.status"), "local_player_ready");
        assert_eq!(runtime_value("client.local_player"), "0x3000");
        clear();
    }
}
