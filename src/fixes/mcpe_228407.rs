// MCPE-228407:
// Mojang modified RakNet's 0x86 handler reads a SystemAddress without
// checking packet length. Drop undersized packets before the vulnerable path.

pub const ID_PONG_ADDRESS_INFO: u8 = 0x86;

// LeviLamina headers show RakNet::SystemAddress is 136 bytes on current builds.
const RAKNET_SYSTEM_ADDRESS_SIZE: usize = 136;
pub const MIN_PONG_ADDRESS_INFO_PACKET_LEN: usize = 1 + RAKNET_SYSTEM_ADDRESS_SIZE;

pub fn should_drop_malformed_offline_packet(payload: &[u8]) -> bool {
    if payload.first().copied() != Some(ID_PONG_ADDRESS_INFO) {
        return false;
    }
    payload.len() < MIN_PONG_ADDRESS_INFO_PACKET_LEN
}
