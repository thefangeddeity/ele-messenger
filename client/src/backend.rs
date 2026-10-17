//! Where the mesh comes from, and the local API it is reached through.
//!
//! If an elemesh daemon already answers on this machine the client uses it.
//! Otherwise the client runs a node of its own, serving the same API on the
//! same address, so everything after start-up is one code path: HTTP to
//! 127.0.0.1:8740.

use anyhow::{Context, Result, bail};
use elemesh::daemon;
use serde::Deserialize;
use std::time::Duration;

pub const API: &str = "http://127.0.0.1:8740";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Starting,
    /// An elemesh daemon on this machine; the client only shows it.
    Daemon,
    /// The client's own node, joined to the mesh.
    Builtin,
    /// The client's own node without the passphrase: listening only.
    Scouting,
}

#[derive(Deserialize, Default, Clone)]
pub struct Status {
    pub node: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub links: Vec<Link>,
    #[serde(default)]
    pub nearby: Vec<Nearby>,
}

#[derive(Deserialize, Clone)]
pub struct Link {
    pub peer: String,
    pub transport: String,
    pub via: String,
    pub heard_s: u64,
}

#[derive(Deserialize, Clone)]
pub struct Nearby {
    pub peer: Option<String>,
    pub transport: String,
    pub via: String,
    pub seen_s: u64,
}

#[derive(Deserialize, Clone)]
pub struct Message {
    pub author: String,
    pub ts_ms: u64,
    pub nick: String,
    pub text: String,
}

#[derive(Deserialize)]
struct Page {
    cursor: i64,
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct Rooms {
    rooms: Vec<String>,
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build().into()
}

fn get_json<T: serde::de::DeserializeOwned>(a: &ureq::Agent, path: &str) -> Result<T> {
    let mut r = a.get(format!("{API}{path}")).call()?;
    if r.status() != 200 {
        bail!("{} from {path}", r.status());
    }
    Ok(r.body_mut().read_json()?)
}

pub fn status() -> Result<Status> {
    get_json(&agent(Duration::from_secs(2)), "/v1/status")
}

pub fn rooms() -> Result<Vec<String>> {
    let r: Rooms = get_json(&agent(Duration::from_secs(2)), "/v1/rooms")?;
    Ok(r.rooms)
}

/// Waits up to `wait` seconds for messages after `after`.
pub fn messages(room: &str, after: i64, wait: u64) -> Result<(i64, Vec<Message>)> {
    let p: Page = get_json(
        &agent(Duration::from_secs(wait + 5)),
        &format!("/v1/rooms/{}/messages?after={after}&wait={wait}", url_part(room)),
    )?;
    Ok((p.cursor, p.messages))
}

pub fn post(room: &str, nick: &str, text: &str) -> Result<()> {
    let mut r = agent(Duration::from_secs(5))
        .post(format!("{API}/v1/rooms/{}/messages", url_part(room)))
        .send_json(serde_json::json!({ "nick": nick, "text": text }))?;
    if r.status() != 200 {
        let v: serde_json::Value = r.body_mut().read_json().unwrap_or_default();
        bail!("{}", v["error"].as_str().unwrap_or("could not send"));
    }
    Ok(())
}

fn url_part(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The client's own node, when it runs one. Dropping it stops the node and
/// frees its ports.
struct Embedded {
    stop: tokio::sync::oneshot::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl Embedded {
    fn start(scout: bool) -> Result<Self> {
        let opts = daemon::Options::new(daemon::state_dir(None)?);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::Builder::new().name("ele-node".into()).spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => return tracing::error!("node runtime: {e}"),
            };
            rt.block_on(async move {
                let run = async { if scout { daemon::scout(opts).await } else { daemon::run(opts).await } };
                tokio::select! {
                    r = run => if let Err(e) = r { tracing::error!("node stopped: {e:#}") },
                    _ = stopped => {}
                }
            });
            rt.shutdown_timeout(Duration::from_secs(2));
        })?;
        Ok(Self { stop, thread })
    }

    fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.thread.join();
    }
}

pub struct Backend {
    pub mode: Mode,
    embedded: Option<Embedded>,
    pub error: Option<String>,
}

impl Backend {
    pub fn new() -> Self {
        Self { mode: Mode::Starting, embedded: None, error: None }
    }

    /// Uses the daemon on this machine if one answers; otherwise starts a node
    /// of our own, joined if this machine has the passphrase, else scouting.
    pub fn connect(&mut self) {
        if status().is_ok() {
            self.mode = Mode::Daemon;
            return;
        }
        let joined = daemon::state_dir(None).map(|d| daemon::has_passphrase(&d)).unwrap_or(false);
        match Embedded::start(!joined) {
            Ok(e) => {
                self.embedded = Some(e);
                self.mode = if joined { Mode::Builtin } else { Mode::Scouting };
            }
            Err(e) => self.error = Some(format!("could not start a node: {e:#}")),
        }
    }

    /// Stores the passphrase and restarts our node as a full member.
    pub fn join(&mut self, passphrase: &str) -> Result<()> {
        let dir = daemon::state_dir(None)?;
        daemon::init(&dir, passphrase)?;
        if let Some(e) = self.embedded.take() {
            e.stop();
        }
        // The scout's sockets close with its runtime; give the OS a moment.
        for _ in 0..20 {
            if status().is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.embedded = Some(Embedded::start(false).context("starting the node")?);
        self.mode = Mode::Builtin;
        Ok(())
    }

    pub fn shutdown(&mut self) {
        if let Some(e) = self.embedded.take() {
            e.stop();
        }
    }
}
