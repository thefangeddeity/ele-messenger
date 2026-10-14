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
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::ble::{self, SERVICE_UUID, STREAM_UUID};
use crate::link;
use crate::node::Node;
use crate::proto::NodeId;

const SCAN_EVERY: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn run(node: Arc<Node>) -> Result<()> {
    let session = bluer::Session::new().await.context("talking to bluetoothd")?;
    let adapter = session.default_adapter().await.context("no Bluetooth adapter")?;
    adapter.set_powered(true).await?;
    tracing::info!("ble: adapter {} ({})", adapter.name(), adapter.address().await?);

    let peripheral = serve(node.clone(), adapter.clone());
    let central = scan(node, adapter);
    tokio::select! {
        r = peripheral => r,
        r = central => r,
    }
}

async fn serve(node: Arc<Node>, adapter: Adapter) -> Result<()> {
    let (control, handle) = characteristic_control();
    let app = Application {
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
    };
    let _app = adapter.serve_gatt_application(app).await.context("registering GATT service")?;
    let _adv = adapter
        .advertise(Advertisement {
            service_uuids: [Uuid::from_u128(SERVICE_UUID)].into_iter().collect(),
            discoverable: Some(true),
            local_name: Some(ble::local_name(&node.identity.id)),
            ..Default::default()
        })
        .await
        .context("starting advertisement")?;
    tracing::info!("ble: advertising as {}", ble::local_name(&node.identity.id));

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

async fn scan(node: Arc<Node>, adapter: Adapter) -> Result<()> {
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
    let mut tick = tokio::time::interval(SCAN_EVERY);
    loop {
        tokio::select! {
            ev = events.next() => if ev.is_none() { bail!("discovery stopped") },
            _ = tick.tick() => {}
        }
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
            let n = node.clone();
            tokio::spawn(async move {
                let result = dial(&n, device).await;
                if let Err(e) = &result {
                    tracing::debug!("ble dial {addr}: {e:#}");
                }
                n.end_dial(key, "ble", result.is_ok());
            });
        }
    }
}

async fn dial(node: &Arc<Node>, device: bluer::Device) -> Result<()> {
    let addr = device.address();
    if !device.is_connected().await? {
        tokio::time::timeout(CONNECT_TIMEOUT, device.connect()).await.context("connect timed out")??;
    }
    let result = async {
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
        let reader = ch.notify_io().await?;
        let writer = ch.write_io().await?;
        link::run(tokio::io::join(reader, writer), true, "ble", addr.to_string(), node.clone()).await
    }
    .await;
    let _ = device.disconnect().await;
    result
}
