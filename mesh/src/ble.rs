//! Shared BLE constants. Every node is GATT peripheral and central at once;
//! one characteristic carries the link byte stream: centrals write to it,
//! peripherals notify on it.


use crate::proto::NodeId;

pub const SERVICE_UUID: u128 = 0xe1e5a3c0_7d1b_4f6a_9c1e_6d2b8a4f0e11;
pub const STREAM_UUID: u128 = 0xe1e5a3c1_7d1b_4f6a_9c1e_6d2b8a4f0e11;
const NAME_PREFIX: &str = "ele-";

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn local_name(id: &NodeId) -> String {
    format!("{NAME_PREFIX}{}", hex::encode(id))
}

pub fn peer_from_name(name: &str) -> Option<NodeId> {
    let hex_id = name.strip_prefix(NAME_PREFIX)?;
    hex::decode(hex_id).ok()?.try_into().ok()
}

/// Lower id is the central, so two full-role nodes don't both dial. A peer
/// whose name we couldn't read is dialled anyway; the link table drops
/// duplicates after the handshake.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn should_dial(me: &NodeId, peer: Option<NodeId>) -> bool {
    match peer {
        Some(p) => p != *me && *me < p,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_roundtrip() {
        let id = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(peer_from_name(&local_name(&id)), Some(id));
        assert_eq!(peer_from_name("ele-zz"), None);
        assert_eq!(peer_from_name("Someone's headphones"), None);
    }
}
