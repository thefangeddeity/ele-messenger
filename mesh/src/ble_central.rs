//! BLE central role via btleplug (macOS, Windows). This side cannot advertise
//! yet, so it dials every ELE node it hears; the link table keeps one link
//! per peer.

use anyhow::{Context, Result};
use btleplug::api::{Central, CentralEvent, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::ble::{self, SERVICE_UUID, STREAM_UUID};
use crate::link;
use crate::node::Node;
use crate::proto::NodeId;

const CHUNK: usize = 180;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn run(node: Arc<Node>) -> Result<()> {
    let manager = Manager::new().await?;
    let adapter = manager.adapters().await?.into_iter().next().context("no Bluetooth adapter")?;
    tracing::info!("ble: central on {}", adapter.adapter_info().await.unwrap_or_default());
    let mut events = adapter.events().await?;
    // ELEMESH_BLE_DEBUG=1 scans unfiltered and logs everything heard, to tell
    // "radio blocked" apart from "peer not advertising".
    let debug = std::env::var_os("ELEMESH_BLE_DEBUG").is_some();
    let filter = if debug { ScanFilter::default() } else { ScanFilter { services: vec![Uuid::from_u128(SERVICE_UUID)] } };
    adapter.start_scan(filter).await?;
    while let Some(ev) = events.next().await {
        let id = match ev {
            CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) => id,
            _ => continue,
        };
        let Ok(p) = adapter.peripheral(&id).await else { continue };
        let Ok(Some(props)) = p.properties().await else { continue };
        if debug {
            tracing::info!("ble heard {id:?} name={:?} rssi={:?} services={:?}", props.local_name, props.rssi, props.services);
        }
        if !props.services.contains(&Uuid::from_u128(SERVICE_UUID)) {
            continue;
        }
        let peer = props.local_name.as_deref().and_then(ble::peer_from_name);
        node.heard(peer, "ble", format!("{id:?}"));
        if peer == Some(node.identity.id) || peer.is_some_and(|x| node.is_linked(&x, "ble")) {
            continue;
        }
        let key = peer.unwrap_or_else(|| dial_key(&format!("{id:?}")));
        if !node.try_begin_dial(key, "ble") {
            continue;
        }
        let n = node.clone();
        let a = adapter.clone();
        tokio::spawn(async move {
            let result = dial(&n, &a, p).await;
            if let Err(e) = &result {
                tracing::debug!("ble dial {id:?}: {e:#}");
            }
            n.end_dial(key, "ble", result.is_ok());
        });
    }
    anyhow::bail!("scan ended")
}

fn dial_key(s: &str) -> NodeId {
    let h = Sha256::digest(s.as_bytes());
    let mut k = [0u8; 8];
    k.copy_from_slice(&h[..8]);
    k
}

async fn dial(node: &Arc<Node>, _adapter: &Adapter, p: Peripheral) -> Result<()> {
    tokio::time::timeout(CONNECT_TIMEOUT, p.connect()).await.context("connect timed out")??;
    let result = async {
        // Any setup step can hang on a confused peer; a dial stuck here
        // would block every later dial to the same node.
        let (ch, mut notes) = tokio::time::timeout(CONNECT_TIMEOUT, async {
            p.discover_services().await?;
            let ch = p
                .characteristics()
                .into_iter()
                .find(|c| c.uuid == Uuid::from_u128(STREAM_UUID))
                .context("peer has no ELE stream characteristic")?;
            p.subscribe(&ch).await?;
            anyhow::Ok((ch.clone(), p.notifications().await?))
        })
        .await
        .context("GATT setup timed out")??;

        // Bridge the characteristic to a byte stream for the link layer.
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let (mut from_link, mut to_link) = tokio::io::split(theirs);
        let pw = p.clone();
        let wch = ch.clone();
        let uplink = async move {
            let mut buf = [0u8; CHUNK];
            loop {
                let n = from_link.read(&mut buf).await?;
                if n == 0 {
                    return anyhow::Ok(());
                }
                pw.write(&wch, &buf[..n], WriteType::WithResponse).await?;
            }
        };
        let downlink = async move {
            while let Some(v) = notes.next().await {
                if v.uuid == Uuid::from_u128(STREAM_UUID) {
                    to_link.write_all(&v.value).await?;
                }
            }
            anyhow::Ok(())
        };
        let label = format!("{:?}", p.id());
        tokio::select! {
            r = link::run(ours, true, "ble", label, node.clone()) => r,
            r = uplink => r,
            r = downlink => r,
        }
    }
    .await;
    let _ = p.disconnect().await;
    result
}
