//! BLE peripheral role on macOS via CoreBluetooth: advertises the ELE service
//! so other nodes can find this Mac and dial it. The central role stays in
//! ble_central (btleplug); both run at once.
//!
//! CoreBluetooth calls the delegate on one serial dispatch queue. Every
//! CoreBluetooth object is touched only there: tokio tasks that need the
//! radio post a closure to the same queue.

use anyhow::{Result, bail};
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_core_bluetooth::{
    CBATTError, CBATTRequest, CBAdvertisementDataLocalNameKey, CBAdvertisementDataServiceUUIDsKey,
    CBAttributePermissions, CBCentral, CBCharacteristic, CBCharacteristicProperties, CBManagerState,
    CBMutableCharacteristic, CBMutableService, CBPeripheralManager, CBPeripheralManagerDelegate, CBService, CBUUID,
};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSError, NSObject, NSObjectProtocol, NSString};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::ble::{self, SERVICE_UUID, STREAM_UUID};
use crate::link;
use crate::node::Node;

/// Notifications queued for one central before it counts as stuck.
const MAX_PENDING: usize = 512;
/// Writes kept from a central that hasn't subscribed yet.
const MAX_EARLY: usize = 16;

pub async fn run(node: Arc<Node>) -> Result<()> {
    let (fatal_tx, mut fatal_rx) = mpsc::unbounded_channel();
    let _radio = Radio::start(node, fatal_tx);
    match fatal_rx.recv().await {
        Some(e) => bail!(e),
        None => bail!("peripheral stopped"),
    }
}

/// One central's link: bytes it writes go to `inbound`, bytes for it wait in
/// `pending` until CoreBluetooth has room.
struct Session {
    central: Retained<CBCentral>,
    generation: u64,
    inbound: mpsc::UnboundedSender<Vec<u8>>,
    pending: VecDeque<Vec<u8>>,
}

#[derive(Default)]
struct State {
    manager: Option<Retained<CBPeripheralManager>>,
    stream: Option<Retained<CBMutableCharacteristic>>,
    sessions: HashMap<String, Session>,
    early: HashMap<String, Vec<Vec<u8>>>,
    generation: u64,
}

struct Ivars {
    node: Arc<Node>,
    rt: tokio::runtime::Handle,
    queue: DispatchRetained<DispatchQueue>,
    fatal: mpsc::UnboundedSender<String>,
    state: RefCell<State>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = AnyThread]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl CBPeripheralManagerDelegate for Delegate {
        #[unsafe(method(peripheralManagerDidUpdateState:))]
        fn did_update_state(&self, manager: &CBPeripheralManager) {
            let state = unsafe { manager.state() };
            if state == CBManagerState::PoweredOn {
                self.add_service(manager);
            } else if state == CBManagerState::Unauthorized {
                self.fail("Bluetooth not allowed for ELE Mesh (Privacy & Security → Bluetooth)");
            } else if state == CBManagerState::PoweredOff {
                self.fail("Bluetooth is off");
            } else if state == CBManagerState::Unsupported {
                self.fail("this Mac cannot act as a Bluetooth peripheral");
            }
        }

        #[unsafe(method(peripheralManager:didAddService:error:))]
        fn did_add_service(&self, manager: &CBPeripheralManager, _service: &CBService, error: Option<&NSError>) {
            if let Some(e) = error {
                return self.fail(&format!("adding GATT service: {}", e.localizedDescription()));
            }
            let name = ble::local_name(&self.ivars().node.identity.id);
            let uuids = NSArray::from_retained_slice(&[cbuuid(SERVICE_UUID)]);
            let name_ns = NSString::from_str(&name);
            let keys: [&NSString; 2] = unsafe { [CBAdvertisementDataServiceUUIDsKey, CBAdvertisementDataLocalNameKey] };
            let values: [&AnyObject; 2] = [uuids.as_ref(), name_ns.as_ref()];
            let adv = NSDictionary::from_slices(&keys, &values);
            unsafe { manager.startAdvertising(Some(&adv)) };
        }

        #[unsafe(method(peripheralManagerDidStartAdvertising:error:))]
        fn did_start_advertising(&self, _manager: &CBPeripheralManager, error: Option<&NSError>) {
            if let Some(e) = error {
                return self.fail(&format!("advertising: {}", e.localizedDescription()));
            }
            // Not ble::set_advertising(true) yet: links *into* a Mac are
            // unverified on hardware, so the Mac keeps dialling everyone
            // (as before it advertised) rather than waiting to be dialled.
            tracing::info!("ble: advertising as {}", ble::local_name(&self.ivars().node.identity.id));
        }

        #[unsafe(method(peripheralManager:central:didSubscribeToCharacteristic:))]
        fn did_subscribe(&self, _manager: &CBPeripheralManager, central: &CBCentral, _ch: &CBCharacteristic) {
            self.open_session(central);
        }

        #[unsafe(method(peripheralManager:central:didUnsubscribeFromCharacteristic:))]
        fn did_unsubscribe(&self, _manager: &CBPeripheralManager, central: &CBCentral, _ch: &CBCharacteristic) {
            let id = central_id(central);
            // Dropping the session closes its inbound side; the link sees EOF.
            self.ivars().state.borrow_mut().sessions.remove(&id);
        }

        #[unsafe(method(peripheralManager:didReceiveWriteRequests:))]
        fn did_receive_writes(&self, manager: &CBPeripheralManager, requests: &NSArray<CBATTRequest>) {
            let mut st = self.ivars().state.borrow_mut();
            for req in requests.iter() {
                let central = unsafe { req.central() };
                let id = central_id(&central);
                let Some(value) = (unsafe { req.value() }) else { continue };
                let bytes = value.to_vec();
                if let Some(s) = st.sessions.get(&id) {
                    let _ = s.inbound.send(bytes);
                } else {
                    let early = st.early.entry(id).or_default();
                    if early.len() < MAX_EARLY {
                        early.push(bytes);
                    }
                }
            }
            drop(st);
            // One response answers the whole batch.
            if let Some(first) = requests.firstObject() {
                unsafe { manager.respondToRequest_withResult(&first, CBATTError::Success) };
            }
        }

        #[unsafe(method(peripheralManagerIsReadyToUpdateSubscribers:))]
        fn ready_to_update(&self, _manager: &CBPeripheralManager) {
            let ids: Vec<String> = self.ivars().state.borrow().sessions.keys().cloned().collect();
            for id in ids {
                self.flush(&id);
            }
        }
    }
);

impl Delegate {
    fn fail(&self, why: &str) {
        ble::set_advertising(false);
        let _ = self.ivars().fatal.send(why.to_string());
    }

    fn add_service(&self, manager: &CBPeripheralManager) {
        let stream = unsafe {
            CBMutableCharacteristic::initWithType_properties_value_permissions(
                CBMutableCharacteristic::alloc(),
                &cbuuid(STREAM_UUID),
                CBCharacteristicProperties::Write
                    | CBCharacteristicProperties::WriteWithoutResponse
                    | CBCharacteristicProperties::Notify,
                None,
                CBAttributePermissions::Writeable,
            )
        };
        let service = unsafe { CBMutableService::initWithType_primary(CBMutableService::alloc(), &cbuuid(SERVICE_UUID), true) };
        let chars: Retained<NSArray<CBCharacteristic>> =
            NSArray::from_retained_slice(&[Retained::into_super(stream.clone())]);
        unsafe {
            service.setCharacteristics(Some(&chars));
            manager.removeAllServices();
            manager.addService(&service);
        }
        let mut st = self.ivars().state.borrow_mut();
        st.stream = Some(stream);
        st.sessions.clear();
    }

    fn open_session(&self, central: &CBCentral) {
        let iv = self.ivars();
        let id = central_id(central);
        let mtu = unsafe { central.maximumUpdateValueLength() }.max(20);
        let (inbound, mut from_central) = mpsc::unbounded_channel::<Vec<u8>>();
        let generation = {
            let mut st = iv.state.borrow_mut();
            st.generation += 1;
            for bytes in st.early.remove(&id).unwrap_or_default() {
                let _ = inbound.send(bytes);
            }
            let generation = st.generation;
            st.sessions.insert(
                id.clone(),
                Session { central: central.retain(), generation, inbound, pending: VecDeque::new() },
            );
            generation
        };

        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let (mut to_central, mut to_link) = tokio::io::split(theirs);
        let home = Home(self.retain());
        let node = iv.node.clone();
        let label = id.clone();
        let _enter = iv.rt.enter();
        tokio::spawn(async move {
            while let Some(bytes) = from_central.recv().await {
                if to_link.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            if let Err(e) = link::run(ours, false, "ble", label, node).await {
                tracing::debug!("ble inbound: {e:#}");
            }
        });
        tokio::spawn(async move {
            let mut buf = vec![0u8; mtu];
            loop {
                let n = match to_central.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let chunk = buf[..n].to_vec();
                let id = id.clone();
                home.post(move |d| d.send(&id, generation, chunk));
            }
            home.post(move |d| d.close(&id, generation));
        });
    }

    fn send(&self, id: &str, generation: u64, chunk: Vec<u8>) {
        {
            let mut st = self.ivars().state.borrow_mut();
            let Some(s) = st.sessions.get_mut(id).filter(|s| s.generation == generation) else { return };
            if s.pending.len() >= MAX_PENDING {
                tracing::debug!("ble: central {id} not reading, dropping it");
                st.sessions.remove(id);
                return;
            }
            s.pending.push_back(chunk);
        }
        self.flush(id);
    }

    fn close(&self, id: &str, generation: u64) {
        let mut st = self.ivars().state.borrow_mut();
        if st.sessions.get(id).is_some_and(|s| s.generation == generation) {
            st.sessions.remove(id);
        }
    }

    /// Sends queued notifications until CoreBluetooth's buffer is full; it
    /// calls ready_to_update when there is room again.
    fn flush(&self, id: &str) {
        let mut st = self.ivars().state.borrow_mut();
        let st = &mut *st;
        let (Some(manager), Some(stream)) = (&st.manager, &st.stream) else { return };
        let Some(s) = st.sessions.get_mut(id) else { return };
        let to = NSArray::from_retained_slice(&[s.central.clone()]);
        while let Some(chunk) = s.pending.front() {
            let data = NSData::with_bytes(chunk);
            if !unsafe { manager.updateValue_forCharacteristic_onSubscribedCentrals(&data, stream, Some(&to)) } {
                break;
            }
            s.pending.pop_front();
        }
    }
}

/// The delegate as seen from tokio tasks. It is only dereferenced inside
/// closures run on the delegate's own serial queue.
struct Home(Retained<Delegate>);
unsafe impl Send for Home {}
unsafe impl Sync for Home {}

impl Home {
    // A method call, so closures capture the whole Home and not the
    // non-Send Retained inside it.
    fn get(&self) -> &Delegate {
        &self.0
    }

    fn post(&self, f: impl FnOnce(&Delegate) + Send + 'static) {
        let me = Home(self.0.clone());
        self.0.ivars().queue.exec_async(move || f(me.get()));
    }
}

/// Owns the peripheral manager; dropping it stops advertising and closes
/// every inbound link.
struct Radio(Home);

impl Radio {
    fn start(node: Arc<Node>, fatal: mpsc::UnboundedSender<String>) -> Radio {
        let queue = DispatchQueue::new("org.ele-messenger.elemesh.ble", None);
        let ivars = Ivars {
            node,
            rt: tokio::runtime::Handle::current(),
            queue: queue.clone(),
            fatal,
            state: RefCell::new(State::default()),
        };
        let delegate: Retained<Delegate> = unsafe { msg_send![super(Delegate::alloc().set_ivars(ivars)), init] };
        let home = Home(delegate);
        let d = Home(home.0.clone());
        // Create the manager on its queue so no delegate call can run before
        // the manager is stored.
        queue.exec_sync(move || {
            let manager = unsafe {
                CBPeripheralManager::initWithDelegate_queue(
                    CBPeripheralManager::alloc(),
                    Some(ProtocolObject::from_ref(d.get())),
                    Some(&d.get().ivars().queue),
                )
            };
            d.get().ivars().state.borrow_mut().manager = Some(manager);
        });
        Radio(home)
    }
}

impl Drop for Radio {
    fn drop(&mut self) {
        ble::set_advertising(false);
        let d = Home(self.0.0.clone());
        self.0.0.ivars().queue.exec_sync(move || {
            let mut st = d.get().ivars().state.borrow_mut();
            if let Some(m) = st.manager.take() {
                unsafe {
                    m.stopAdvertising();
                    m.removeAllServices();
                    m.setDelegate(None);
                }
            }
            st.sessions.clear();
            st.early.clear();
        });
    }
}

fn cbuuid(u: u128) -> Retained<CBUUID> {
    unsafe { CBUUID::UUIDWithString(&NSString::from_str(&uuid::Uuid::from_u128(u).to_string())) }
}

fn central_id(c: &CBCentral) -> String {
    unsafe { c.identifier() }.UUIDString().to_string()
}
