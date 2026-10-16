//! BLE transport on Linux via BlueZ: advertises a GATT service and scans for
//! other nodes at the same time.

use anyhow::{Context, Result, bail};
use bluer::adv::Advertisement;
use bluer::gatt::local::{
    Application, Characteristic, CharacteristicControlEvent, CharacteristicNotify, CharacteristicNotifyMethod,
    CharacteristicWrite, CharacteristicWriteMethod, Service, characteristic_control,
};
use bluer::gatt::{CharacteristicReader, CharacteristicWriter};
use bluer::{Adapter, Address, DiscoveryFilter, DiscoveryTransport, Uuid};
use futures::{StreamExt, pin_mut};
use rand::RngExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::ble::{self, SERVICE_UUID, STREAM_UUID};
use crate::link;
use crate::node::Node;
use crate::proto::NodeId;

const SCAN_WINDOW: Duration = Duration::from_secs(6);
const SCAN_CHECK: Duration = Duration::from_secs(2);
const SCAN_IDLE: Duration = Duration::from_secs(9);
const STALE_AFTER: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn run(node: Arc<Node>) -> Result<()> {
    let session = bluer::Session::new().await.context("talking to bluetoothd")?;
    let adapter = session.default_adapter().await.context("no Bluetooth adapter")?;
    adapter.set_powered(true).await?;
    tracing::info!("ble: adapter {} ({})", adapter.name(), adapter.address().await?);

    // The GATT service must be registered before the first advertisement.
    let (control, handle) = characteristic_control();
    let _app = adapter.serve_gatt_application(gatt_app(handle)).await.context("registering GATT service")?;
    let peripheral = serve(node.clone(), control);
    let central = rendezvous(node, adapter);
    tokio::select! {
        r = peripheral => r,
        r = central => r,
    }
}

fn gatt_app(handle: bluer::gatt::local::CharacteristicControlHandle) -> Application {
    Application {
        services: vec![Service {
            uuid: Uuid::from_u128(SERVICE_UUID),
            primary: true,
            characteristics: vec![Characteristic {
                uuid: Uuid::from_u128(STREAM_UUID),
                write: Some(CharacteristicWrite {
                    write: true,
                    write_without_response: true,
                    method: CharacteristicWriteMethod::Io,
                    ..Default::default()
                }),
                notify: Some(CharacteristicNotify {
                    notify: true,
                    method: CharacteristicNotifyMethod::Io,
                    ..Default::default()
                }),
                control_handle: handle,
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

async fn serve(node: Arc<Node>, control: bluer::gatt::local::CharacteristicControl) -> Result<()> {
    // A central opens the stream in two halves (write, then subscribe, or the
    // reverse); pair them by address before starting the link.
    let mut halves: HashMap<Address, (Option<CharacteristicReader>, Option<CharacteristicWriter>)> = HashMap::new();
    pin_mut!(control);
    while let Some(event) = control.next().await {
        let addr = match event {
            CharacteristicControlEvent::Write(req) => {
                let reader = req.accept()?;
                let addr = reader.device_address();
                halves.entry(addr).or_default().0 = Some(reader);
                addr
            }
            CharacteristicControlEvent::Notify(writer) => {
                let addr = writer.device_address();
                halves.entry(addr).or_default().1 = Some(writer);
                addr
            }
        };
        if let Some((Some(_), Some(_))) = halves.get(&addr) {
            let (r, w) = halves.remove(&addr).unwrap();
            let stream = tokio::io::join(r.unwrap(), w.unwrap());
            let n = node.clone();
            tokio::spawn(async move {
                if let Err(e) = link::run(stream, false, "ble", addr.to_string(), n).await {
                    tracing::debug!("ble inbound {addr}: {e:#}");
                }
            });
        }
    }
    bail!("GATT service closed")
}

fn addr_key(addr: Address) -> NodeId {
    let mut k = [0xffu8; 8];
    k[..6].copy_from_slice(&addr.0);
    k
}

/// Alternates advertising and short scan windows, and never scans while any
/// LE connection or BLE dial is up. Realtek RTL8821C combo chips time out on
/// "LE set scan enable" (on or off) while connected and reset over USB,
/// dropping every link; not advertising while scanning means nobody can
/// connect mid-window. The idle phase is jittered so two nodes don't lock
/// into the same rhythm and never hear each other. Adapter errors return,
/// so the transport restarts on whatever adapter comes back.
async fn rendezvous(node: Arc<Node>, adapter: Adapter) -> Result<()> {
    let name = ble::local_name(&node.identity.id);
    tracing::info!("ble: advertising as {name}");
    let mut adv = None;
    let mut stale = HashMap::new();
    loop {
        if adv.is_none() && !node.scout {
            let a = Advertisement {
                service_uuids: [Uuid::from_u128(SERVICE_UUID)].into_iter().collect(),
                discoverable: Some(true),
                local_name: Some(name.clone()),
                ..Default::default()
            };
            // Some controllers refuse a connectable advertisement while they
            // hold a connection; try again next turn rather than giving up.
            match adapter.advertise(a).await {
                Ok(h) => adv = Some(h),
                Err(e) => tracing::debug!("ble advertise: {e}"),
            }
        }
        let jitter = Duration::from_millis(rand::rng().random_range(0..6000));
        tokio::time::sleep(SCAN_IDLE + jitter).await;
        reap_stale(&node, &adapter, &mut stale).await?;
        if node.transport_busy("ble") || any_connected(&adapter).await? {
            continue;
        }
        adv = None; // stop advertising for the scan window
        {
            match scan_window(&node, &adapter).await {
                Ok(found) => {
                    for (key, device) in found {
                        let n = node.clone();
                        tokio::spawn(async move {
                            let addr = device.address();
                            let result = dial(&n, device).await;
                            if let Err(e) = &result {
                                tracing::debug!("ble dial {addr}: {e:#}");
                            }
                            n.end_dial(key, "ble", result.is_ok());
                        });
                    }
                }
                Err(e) => tracing::debug!("ble scan: {e:#}"),
            }
        }
        // Fails when the adapter is gone (USB reset renames hci0 to hci1).
        adapter.is_powered().await.context("adapter lost")?;
    }
}

/// Drops connections that carry no link: a peer whose daemon died, or one
/// left over from before our restart, keeps the radio connected and so
/// blocks both advertising and scanning. ELE peers never pair, so paired
/// devices (keyboards, headphones) are left alone.
async fn reap_stale(node: &Node, adapter: &Adapter, since: &mut HashMap<Address, std::time::Instant>) -> Result<()> {
    let linked: Vec<String> = node.links().into_iter().filter(|l| l.transport == "ble").map(|l| l.via).collect();
    let mut seen = Vec::new();
    for addr in adapter.device_addresses().await? {
        let device = adapter.device(addr)?;
        if !device.is_connected().await.unwrap_or(false)
            || device.is_paired().await.unwrap_or(true)
            || linked.contains(&addr.to_string())
        {
            continue;
        }
        seen.push(addr);
        let first = *since.entry(addr).or_insert_with(std::time::Instant::now);
        if first.elapsed() >= STALE_AFTER {
            tracing::info!("ble: dropping idle connection from {addr}");
            let _ = device.disconnect().await;
            since.remove(&addr);
        }
    }
    since.retain(|a, _| seen.contains(a));
    Ok(())
}

async fn any_connected(adapter: &Adapter) -> Result<bool> {
    for addr in adapter.device_addresses().await? {
        if adapter.device(addr)?.is_connected().await.unwrap_or(false) {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn scan_window(node: &Arc<Node>, adapter: &Adapter) -> Result<Vec<(NodeId, bluer::Device)>> {
    adapter
        .set_discovery_filter(DiscoveryFilter {
            uuids: [Uuid::from_u128(SERVICE_UUID)].into_iter().collect(),
            transport: DiscoveryTransport::Le,
            ..Default::default()
        })
        .await?;
    // Discovery runs while this stream is alive.
    let events = adapter.discover_devices().await?;
    pin_mut!(events);
    let window = tokio::time::sleep(SCAN_WINDOW);
    pin_mut!(window);
    let mut tick = tokio::time::interval(SCAN_CHECK);
    loop {
        tokio::select! {
            ev = events.next() => if ev.is_none() { bail!("discovery stopped") },
            _ = &mut window => return Ok(Vec::new()),
            _ = tick.tick() => {}
        }
        let mut found = Vec::new();
        for addr in adapter.device_addresses().await? {
            let Ok(device) = adapter.device(addr) else { continue };
            // RSSI is only set for devices heard in the current discovery.
            if device.rssi().await.ok().flatten().is_none() {
                continue;
            }
            let uuids = device.uuids().await.ok().flatten().unwrap_or_default();
            if !uuids.contains(&Uuid::from_u128(SERVICE_UUID)) {
                continue;
            }
            let name = device.name().await.ok().flatten();
            let peer = name.as_deref().and_then(ble::peer_from_name);
            node.heard(peer, "ble", addr.to_string());
            if !ble::should_dial(&node.identity.id, peer) {
                continue;
            }
            if let Some(p) = peer {
                if node.is_linked(&p, "ble") {
                    continue;
                }
            }
            let key = peer.unwrap_or_else(|| addr_key(addr));
            if !node.try_begin_dial(key, "ble") {
                continue;
            }
            found.push((key, device));
        }
        if !found.is_empty() {
            // Returning drops the stream, so discovery is off before we connect.
            return Ok(found);
        }
    }
}

async fn dial(node: &Arc<Node>, device: bluer::Device) -> Result<()> {
    let addr = device.address();
    if !device.is_connected().await? {
        tokio::time::timeout(CONNECT_TIMEOUT, device.connect()).await.context("connect timed out")??;
    }
    let setup = async {
        for _ in 0..50 {
            if device.is_services_resolved().await? {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let mut stream_char = None;
        for service in device.services().await? {
            if service.uuid().await? != Uuid::from_u128(SERVICE_UUID) {
                continue;
            }
            for ch in service.characteristics().await? {
                if ch.uuid().await? == Uuid::from_u128(STREAM_UUID) {
                    stream_char = Some(ch);
                }
            }
        }
        let ch = stream_char.context("peer has no ELE stream characteristic")?;
        anyhow::Ok((ch.notify_io().await?, ch.write_io().await?))
    };
    let result = async {
        // A dial stuck in setup would block every later dial to this node.
        let (reader, writer) = tokio::time::timeout(CONNECT_TIMEOUT, setup).await.context("GATT setup timed out")??;
        link::run(tokio::io::join(reader, writer), true, "ble", addr.to_string(), node.clone()).await
    }
    .await;
    let _ = device.disconnect().await;
    result
}
