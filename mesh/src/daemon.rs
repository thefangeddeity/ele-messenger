//! Starting a node: the `elemesh run` daemon and any app that embeds one.

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::node::Node;
use crate::proto::NodeId;
use crate::{api, identity, lan, store};

pub const DEFAULT_API: &str = "127.0.0.1:8740";
pub const DEFAULT_LAN_PORT: u16 = 47474;

pub struct Options {
    pub state_dir: PathBuf,
    pub api: SocketAddr,
    pub lan_port: u16,
    pub lan: bool,
    pub ble: bool,
}

impl Options {
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir, api: DEFAULT_API.parse().unwrap(), lan_port: DEFAULT_LAN_PORT, lan: true, ble: true }
    }
}

/// The per-user state directory every node on this machine shares.
pub fn state_dir(over: Option<PathBuf>) -> Result<PathBuf> {
    let dir = match over {
        Some(d) => d,
        None => dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .context("no state directory; pass --state-dir")?
            .join("elemesh"),
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn has_passphrase(dir: &Path) -> bool {
    std::env::var_os("ELEMESH_PASSPHRASE").is_some() || dir.join("passphrase").exists()
}

fn read_passphrase(dir: &Path) -> Result<String> {
    if let Ok(p) = std::env::var("ELEMESH_PASSPHRASE") {
        return Ok(p);
    }
    let path = dir.join("passphrase");
    let p = std::fs::read_to_string(&path)
        .with_context(|| format!("no mesh passphrase: run `elemesh init --passphrase …` (looked in {})", path.display()))?;
    Ok(p.trim_end_matches(['\r', '\n']).to_string())
}

/// Stores the mesh passphrase and makes sure this machine has an identity.
pub fn init(dir: &Path, passphrase: &str) -> Result<NodeId> {
    identity::derive_psk(passphrase)?;
    identity::write_private(&dir.join("passphrase"), passphrase.as_bytes())?;
    Ok(identity::Identity::load_or_create(dir)?.id)
}

/// This machine's name for messages sent without a nick: the one set with
/// `elemesh name`, else the host name without a local domain.
pub fn name(dir: &Path) -> String {
    let chosen = std::fs::read_to_string(dir.join("name")).ok().map(|s| s.trim().to_string());
    let name = chosen.filter(|s| !s.is_empty()).unwrap_or_else(|| {
        let host = hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
        host.split('.').next().unwrap_or_default().to_string()
    });
    let mut end = name.len().min(crate::store::MAX_NICK);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_string()
}

pub fn set_name(dir: &Path, name: &str) -> Result<()> {
    let name = name.trim();
    anyhow::ensure!(name.len() <= crate::store::MAX_NICK, "name up to {} bytes", crate::store::MAX_NICK);
    let path = dir.join("name");
    if name.is_empty() {
        let _ = std::fs::remove_file(path);
    } else {
        std::fs::write(path, name)?;
    }
    Ok(())
}

/// Runs a full node until the local API stops.
pub async fn run(opts: Options) -> Result<()> {
    let psk = identity::derive_psk(&read_passphrase(&opts.state_dir)?)?;
    let me = identity::Identity::load_or_create(&opts.state_dir)?;
    let store = store::Store::open(&opts.state_dir.join("messages.db"))?;
    let mut node = Node::new(me, psk, store);
    node.name = name(&opts.state_dir);
    let node = Arc::new(node);
    tracing::info!("node {} state {}", node.id_hex(), opts.state_dir.display());
    let n = node.clone();
    tokio::spawn(async move { n.maintenance().await });
    start(node, opts).await
}

/// Runs a node without the passphrase: it listens on every transport and
/// reports who is around, but never advertises, dials or links.
pub async fn scout(opts: Options) -> Result<()> {
    let me = identity::Identity::load_or_create(&opts.state_dir)?;
    let mut node = Node::scout(me, store::Store::in_memory()?);
    node.name = name(&opts.state_dir);
    let node = Arc::new(node);
    tracing::info!("scouting as {} (no passphrase yet)", node.id_hex());
    start(node, opts).await
}

async fn start(node: Arc<Node>, opts: Options) -> Result<()> {
    crate::servers::browse(node.servers.clone(), || {});
    if opts.lan {
        let n = node.clone();
        let port = opts.lan_port;
        supervise("lan", move || lan::run(n.clone(), port));
    }
    if opts.ble {
        spawn_ble(node.clone());
    }
    api::serve(node, opts.api).await
}

/// Keeps a transport running: restarts it 10 s after it fails, backing off
/// to once a minute while it keeps failing the same way (no adapter on this
/// machine), and warns once per distinct error rather than every retry.
fn supervise<F, Fut>(name: &'static str, start: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send,
{
    tokio::spawn(async move {
        let mut last = String::new();
        loop {
            let err = match start().await {
                Ok(()) => String::from("stopped"),
                Err(e) => format!("{e:#}"),
            };
            let delay = if err == last {
                tracing::debug!("{name} transport still down: {err}");
                Duration::from_secs(60)
            } else {
                tracing::warn!("{name} transport down, retrying: {err}");
                Duration::from_secs(10)
            };
            last = err;
            tokio::time::sleep(delay).await;
        }
    });
}

#[cfg(target_os = "linux")]
fn spawn_ble(node: Arc<Node>) {
    supervise("ble", move || crate::ble_linux::run(node.clone()));
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn spawn_ble(node: Arc<Node>) {
    supervise("ble", move || crate::ble_central::run(node.clone()));
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn spawn_ble(_node: Arc<Node>) {
    tracing::warn!("ble: not supported on this platform; using LAN only");
}
