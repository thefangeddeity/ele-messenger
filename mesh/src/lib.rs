//! elemesh as a library, so an app can carry its own node when no daemon
//! runs on the machine.

mod api;
mod ble;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod ble_central;
#[cfg(target_os = "linux")]
mod ble_linux;
#[cfg(target_os = "macos")]
mod ble_mac;
#[cfg(target_os = "windows")]
mod ble_windows;
pub mod daemon;
mod identity;
mod lan;
mod link;
mod node;
mod proto;
mod sign;
mod store;
