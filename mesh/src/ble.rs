//! Shared BLE constants. Every node is GATT peripheral and central at once;
//! one characteristic carries the link byte stream: centrals write to it,
//! peripherals notify on it.

use crate::proto::NodeId;
use std::sync::atomic::{AtomicBool, Ordering};

pub const SERVICE_UUID: u128 = 0xe1e5a3c0_7d1b_4f6a_9c1e_6d2b8a4f0e11;
pub const STREAM_UUID: u128 = 0xe1e5a3c1_7d1b_4f6a_9c1e_6d2b8a4f0e11;
const NAME_PREFIX: &str = "ele-";

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub fn local_name(id: &NodeId) -> String {
    format!("{NAME_PREFIX}{}", hex::encode(id))
}

pub fn peer_from_name(name: &str) -> Option<NodeId> {
    let hex_id = name.strip_prefix(NAME_PREFIX)?;
    hex::decode(hex_id).ok()?.try_into().ok()
}

static ADVERTISING: AtomicBool = AtomicBool::new(cfg!(target_os = "linux"));

/// Whether other nodes can find this one. Linux always serves the GATT
/// service; on macOS it holds once CoreBluetooth confirms advertising.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn set_advertising(on: bool) {
    ADVERTISING.store(on, Ordering::Relaxed);
}

/// Lower id is the central, so two full-role nodes don't both dial. A node
/// nobody can find (not advertising) dials everyone, and a peer whose name
/// we couldn't read is dialled anyway; the link table drops duplicates
/// after the handshake.
pub fn should_dial(me: &NodeId, peer: Option<NodeId>) -> bool {
    match peer {
        Some(p) => p != *me && (*me < p || !ADVERTISING.load(Ordering::Relaxed)),
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
