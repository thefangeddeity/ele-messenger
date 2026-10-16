//! BLE peripheral role on Windows: advertises the ELE service and takes links
//! from centrals, so a Windows machine can be found, not only find. Same GATT
//! shape as Linux: one stream characteristic, the central writes to it and
//! the peripheral notifies on it. Windows can't set the advertised name, so
//! centrals dial it unnamed and learn who it is in the handshake.

use anyhow::{Result, bail};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use windows::Devices::Bluetooth::BluetoothError;
use windows::Devices::Bluetooth::GenericAttributeProfile::{
    GattCharacteristicProperties, GattLocalCharacteristic, GattLocalCharacteristicParameters, GattProtectionLevel,
    GattServiceProvider, GattServiceProviderAdvertisingParameters, GattSession, GattSubscribedClient,
    GattWriteOption, GattWriteRequestedEventArgs,
};
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::{DataReader, DataWriter, IBuffer};
use windows::core::{GUID, Ref};

use crate::ble::{SERVICE_UUID, STREAM_UUID};
use crate::link;
use crate::node::Node;

enum Event {
    Write(String, Vec<u8>),
    Clients(Vec<(String, GattSubscribedClient)>),
}

/// One central's link: bytes it wrote go to `to_link`; `client` is set once
/// it subscribes, which is what notifications are sent to.
struct Peer {
    to_link: mpsc::UnboundedSender<Vec<u8>>,
    client: Arc<Mutex<Option<GattSubscribedClient>>>,
}

pub async fn run(node: Arc<Node>) -> Result<()> {
    let made = GattServiceProvider::CreateAsync(GUID::from_u128(SERVICE_UUID))?.await?;
    if made.Error()? != BluetoothError::Success {
        bail!("GATT service unavailable: {:?} (is Bluetooth on?)", made.Error()?);
    }
    let provider = made.ServiceProvider()?;
    let params = GattLocalCharacteristicParameters::new()?;
    params.SetCharacteristicProperties(
        GattCharacteristicProperties::Write | GattCharacteristicProperties::WriteWithoutResponse | GattCharacteristicProperties::Notify,
    )?;
    params.SetWriteProtectionLevel(GattProtectionLevel::Plain)?;
    params.SetReadProtectionLevel(GattProtectionLevel::Plain)?;
    let ch = provider.Service()?.CreateCharacteristicAsync(GUID::from_u128(STREAM_UUID), &params)?.await?.Characteristic()?;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let t = tx.clone();
    ch.WriteRequested(&TypedEventHandler::new(move |_, args: Ref<GattWriteRequestedEventArgs>| {
        let args = args.ok()?;
        let deferral = args.GetDeferral()?;
        let device = device_of(&args.Session()?)?;
        let req = args.GetRequestAsync()?.join()?;
        let value = bytes(&req.Value()?)?;
        if req.Option()? == GattWriteOption::WriteWithResponse {
            req.Respond()?;
        }
        deferral.Complete()?;
        let _ = t.send(Event::Write(device, value));
        Ok(())
    }))?;
    let t = tx;
    ch.SubscribedClientsChanged(&TypedEventHandler::new(move |sender: Ref<GattLocalCharacteristic>, _| {
        let mut list = Vec::new();
        for c in sender.ok()?.SubscribedClients()? {
            list.push((device_of(&c.Session()?)?, c));
        }
        let _ = t.send(Event::Clients(list));
        Ok(())
    }))?;

    let adv = GattServiceProviderAdvertisingParameters::new()?;
    adv.SetIsDiscoverable(true)?;
    adv.SetIsConnectable(true)?;
    provider.StartAdvertisingWithParameters(&adv)?;
    tracing::info!("ble: advertising the ELE service");
    crate::ble::set_advertising(true);

    let mut peers: HashMap<String, Peer> = HashMap::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            Event::Write(dev, v) => {
                let p = peers.entry(dev.clone()).or_insert_with(|| start(&node, &ch, dev));
                let _ = p.to_link.send(v);
            }
            Event::Clients(list) => {
                for (dev, c) in &list {
                    let p = peers.entry(dev.clone()).or_insert_with(|| start(&node, &ch, dev.clone()));
                    *p.client.lock().unwrap() = Some(c.clone());
                }
                // A central that had subscribed and no longer does has gone:
                // dropping its sender ends its link.
                peers.retain(|dev, p| p.client.lock().unwrap().is_none() || list.iter().any(|(d, _)| d == dev));
            }
        }
    }
    crate::ble::set_advertising(false);
    let _ = provider.StopAdvertising();
    bail!("GATT service closed")
}

fn start(node: &Arc<Node>, ch: &GattLocalCharacteristic, dev: String) -> Peer {
    let (ours, theirs) = tokio::io::duplex(64 * 1024);
    let (mut from_link, mut to_link) = tokio::io::split(theirs);
    let (wtx, mut wrx) = mpsc::unbounded_channel::<Vec<u8>>();
    let client = Arc::new(Mutex::new(None::<GattSubscribedClient>));
    let (c, ch, n) = (client.clone(), ch.clone(), node.clone());
    tokio::spawn(async move {
        let downlink = async {
            while let Some(v) = wrx.recv().await {
                to_link.write_all(&v).await?;
            }
            anyhow::Ok(())
        };
        let uplink = async {
            let mut buf = vec![0u8; 512];
            loop {
                // Bound first: in a let-else the lock guard would live on
                // through the sleep below.
                let subscribed = c.lock().unwrap().clone();
                let Some(cl) = subscribed else {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                let chunk = (cl.Session()?.MaxPduSize()? as usize).saturating_sub(3).clamp(20, buf.len());
                let k = from_link.read(&mut buf[..chunk]).await?;
                if k == 0 {
                    return anyhow::Ok(());
                }
                // An IBuffer isn't Send: let it go before awaiting.
                let sent = {
                    let b = buffer(&buf[..k])?;
                    ch.NotifyValueForSubscribedClientAsync(&b, &cl)?
                };
                sent.await?;
            }
        };
        let r = tokio::select! {
            r = link::run(ours, false, "ble", dev.clone(), n) => r,
            r = uplink => r,
            r = downlink => r,
        };
        if let Err(e) = r {
            tracing::debug!("ble inbound {dev}: {e:#}");
        }
    });
    Peer { to_link: wtx, client }
}

fn device_of(s: &GattSession) -> windows::core::Result<String> {
    Ok(s.DeviceId()?.Id()?.to_string())
}

fn bytes(buf: &IBuffer) -> windows::core::Result<Vec<u8>> {
    let r = DataReader::FromBuffer(buf)?;
    let mut v = vec![0u8; buf.Length()? as usize];
    r.ReadBytes(&mut v)?;
    Ok(v)
}

fn buffer(b: &[u8]) -> windows::core::Result<IBuffer> {
    let w = DataWriter::new()?;
    w.WriteBytes(b)?;
    w.DetachBuffer()
}
