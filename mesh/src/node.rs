use anyhow::{Result, bail};
use rand::RngExt;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, mpsc};

use crate::identity::Identity;
use crate::proto::{DEFAULT_TTL, MAX_PLAINTEXT, NodeId, PROTOCOL_VERSION, Wire};
use crate::store::{Entry, MAX_NICK, MAX_ROOM, MAX_TEXT, Store};

pub const SYNC_INTERVAL: Duration = Duration::from_secs(15);
pub const RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

struct LinkInfo {
    peer: NodeId,
    initiator: bool,
    transport: &'static str,
    label: String,
    since: Instant,
    last_rx: Instant,
    tx: mpsc::Sender<Vec<u8>>,
}

#[derive(Serialize)]
pub struct LinkStatus {
    pub peer: String,
    pub transport: &'static str,
    pub via: String,
    pub initiator: bool,
    pub up_s: u64,
    /// Seconds since the last frame from this peer.
    pub heard_s: u64,
}

pub struct Node {
    pub identity: Identity,
    pub psk: [u8; 32],
    store: Mutex<Store>,
    links: Mutex<HashMap<u64, LinkInfo>>,
    next_link: AtomicU64,
    dials: Mutex<HashMap<(NodeId, &'static str), Dial>>,
    pub changed: Notify,
}

enum Dial {
    InProgress,
    FailedAt(Instant),
}

pub const DIAL_BACKOFF: Duration = Duration::from_secs(60);

impl Node {
    pub fn new(identity: Identity, psk: [u8; 32], store: Store) -> Self {
        Self {
            identity,
            psk,
            store: Mutex::new(store),
            links: Mutex::new(HashMap::new()),
            next_link: AtomicU64::new(1),
            dials: Mutex::new(HashMap::new()),
            changed: Notify::new(),
        }
    }

    /// False while a dial to this peer is running or recently failed.
    pub fn try_begin_dial(&self, peer: NodeId, transport: &'static str) -> bool {
        let mut d = self.dials.lock().unwrap();
        match d.get(&(peer, transport)) {
            Some(Dial::InProgress) => false,
            Some(Dial::FailedAt(t)) if t.elapsed() < DIAL_BACKOFF => false,
            _ => {
                d.insert((peer, transport), Dial::InProgress);
                true
            }
        }
    }

    pub fn end_dial(&self, peer: NodeId, transport: &'static str, ok: bool) {
        let mut d = self.dials.lock().unwrap();
        if ok {
            d.remove(&(peer, transport));
        } else {
            d.insert((peer, transport), Dial::FailedAt(Instant::now()));
        }
    }

    pub fn id_hex(&self) -> String {
        hex::encode(self.identity.id)
    }

    pub fn with_store<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        f(&self.store.lock().unwrap())
    }

    pub fn is_linked(&self, peer: &NodeId, transport: &str) -> bool {
        self.links.lock().unwrap().values().any(|l| &l.peer == peer && l.transport == transport)
    }

    /// A link or dial on this transport is up or under way.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn transport_busy(&self, transport: &str) -> bool {
        self.links.lock().unwrap().values().any(|l| l.transport == transport)
            || self.dials.lock().unwrap().iter().any(|((_, t), d)| *t == transport && matches!(d, Dial::InProgress))
    }

    /// One link per peer per transport. When both sides dialled, both keep the
    /// link whose initiator has the lower node id, so they agree without talking.
    pub fn add_link(
        &self,
        peer: NodeId,
        initiator: bool,
        transport: &'static str,
        label: String,
        tx: mpsc::Sender<Vec<u8>>,
    ) -> Option<u64> {
        let me = self.identity.id;
        let dialer = |init: bool| if init { me } else { peer };
        let mut links = self.links.lock().unwrap();
        if let Some((&old_id, old)) = links.iter().find(|(_, l)| l.peer == peer && l.transport == transport) {
            if dialer(old.initiator) <= dialer(initiator) {
                return None;
            }
            links.remove(&old_id);
        }
        let id = self.next_link.fetch_add(1, Ordering::Relaxed);
        let greeting = [Wire::Hello { version: PROTOCOL_VERSION }, Wire::Summary(self.summary())];
        for w in greeting {
            if let Ok(b) = w.encode() {
                let _ = tx.try_send(b);
            }
        }
        let now = Instant::now();
        links.insert(id, LinkInfo { peer, initiator, transport, label, since: now, last_rx: now, tx });
        Some(id)
    }

    pub fn remove_link(&self, id: u64) {
        self.links.lock().unwrap().remove(&id);
    }

    pub fn links(&self) -> Vec<LinkStatus> {
        self.links
            .lock()
            .unwrap()
            .values()
            .map(|l| LinkStatus {
                peer: hex::encode(l.peer),
                transport: l.transport,
                via: l.label.clone(),
                initiator: l.initiator,
                up_s: l.since.elapsed().as_secs(),
                heard_s: l.last_rx.elapsed().as_secs(),
            })
            .collect()
    }

    fn summary(&self) -> Vec<crate::proto::AuthorState> {
        self.with_store(|s| s.summary()).unwrap_or_default()
    }

    fn send(&self, link: u64, msg: &Wire) {
        let Ok(bytes) = msg.encode() else { return };
        if let Some(l) = self.links.lock().unwrap().get(&link) {
            if l.tx.try_send(bytes).is_err() {
                tracing::warn!("send queue full to {}, dropping frame", hex::encode(l.peer));
            }
        }
    }

    fn broadcast(&self, except: Option<u64>, msg: &Wire) {
        let Ok(bytes) = msg.encode() else { return };
        for (id, l) in self.links.lock().unwrap().iter() {
            if Some(*id) != except {
                let _ = l.tx.try_send(bytes.clone());
            }
        }
    }

    fn send_entries(&self, link: u64, ttl: u8, entries: Vec<Entry>) {
        for batch in batches(entries) {
            self.send(link, &Wire::Entries { ttl, entries: batch });
        }
    }

    pub fn handle(self: &Arc<Self>, link: u64, msg: Wire) {
        if let Some(l) = self.links.lock().unwrap().get_mut(&link) {
            l.last_rx = Instant::now();
        }
        match msg {
            Wire::Hello { version } => {
                if version != PROTOCOL_VERSION {
                    tracing::warn!("peer speaks protocol v{version}, we speak v{PROTOCOL_VERSION}");
                }
            }
            Wire::Summary(theirs) => {
                let missing = self.with_store(|s| s.missing_for(&theirs, usize::MAX)).unwrap_or_default();
                if !missing.is_empty() {
                    // Sync replies are not relayed further; neighbours sync on their own.
                    self.send_entries(link, 1, missing);
                }
            }
            Wire::Entries { ttl, entries } => {
                let received = now_ms();
                let fresh: Vec<Entry> = entries
                    .into_iter()
                    .filter(|e| e.is_well_formed())
                    .filter(|e| self.with_store(|s| s.insert(e, received)).unwrap_or(false))
                    .collect();
                if fresh.is_empty() {
                    return;
                }
                self.changed.notify_waiters();
                if ttl > 1 {
                    let jitter = Duration::from_millis(rand::rng().random_range(10..=220));
                    let node = self.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(jitter).await;
                        for batch in batches(fresh) {
                            node.broadcast(Some(link), &Wire::Entries { ttl: ttl - 1, entries: batch });
                        }
                    });
                }
            }
        }
    }

    pub fn post(&self, room: &str, nick: &str, text: &str, said_at: Option<u64>) -> Result<Entry> {
        let room = room.trim();
        if room.is_empty() || room.len() > MAX_ROOM || nick.len() > MAX_NICK || text.len() > MAX_TEXT || text.is_empty() {
            bail!("room 1-{MAX_ROOM} bytes, nick up to {MAX_NICK}, text 1-{MAX_TEXT}");
        }
        // A caller may know when the message was said better than we do -- it
        // may have been sitting in a store while the radios were off, which is
        // the whole point of store-and-hold. An older stamp is therefore
        // ordinary, and accepted as given.
        //
        // A stamp in the FUTURE is not: ts_ms drives both ordering and the
        // retention cutoff, so one far enough ahead would sort above every
        // real message and never be pruned. Clamped rather than rejected,
        // because a peer with a skewed clock should still be heard.
        let now = now_ms();
        let ts_ms = said_at.map(|t| t.min(now)).unwrap_or(now);
        let entry = self.with_store(|s| {
            let e = Entry {
                author: self.identity.id,
                seq: s.next_seq(&self.identity.id)?,
                ts_ms,
                room: room.to_string(),
                nick: nick.to_string(),
                text: text.to_string(),
            };
            // The second argument is when WE saw it, which is now regardless
            // of when it was said. Local arrival drives the replication
            // cursor; it is not the message's timestamp and never was.
            s.insert(&e, now)?;
            Ok(e)
        })?;
        self.changed.notify_waiters();
        self.broadcast(None, &Wire::Entries { ttl: DEFAULT_TTL, entries: vec![entry.clone()] });
        Ok(entry)
    }

    pub async fn maintenance(&self) {
        let mut tick = tokio::time::interval(SYNC_INTERVAL);
        let mut last_prune = Instant::now() - Duration::from_secs(3600);
        loop {
            tick.tick().await;
            self.broadcast(None, &Wire::Summary(self.summary()));
            if last_prune.elapsed() >= Duration::from_secs(3600) {
                last_prune = Instant::now();
                let cutoff = now_ms().saturating_sub(RETENTION.as_millis() as u64);
                match self.with_store(|s| s.prune(cutoff)) {
                    Ok(0) => {}
                    Ok(n) => tracing::info!("pruned {n} entries older than retention"),
                    Err(e) => tracing::warn!("prune failed: {e}"),
                }
            }
        }
    }
}

fn batches(entries: Vec<Entry>) -> Vec<Vec<Entry>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut size = 0;
    for e in entries {
        let sz = 48 + e.room.len() + e.nick.len() + e.text.len();
        if size + sz > MAX_PLAINTEXT && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            size = 0;
        }
        size += sz;
        cur.push(e);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
