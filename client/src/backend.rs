//! Where the mesh comes from, and the local API it is reached through.
//!
//! If an elemesh daemon already answers on this machine the client uses it.
//! Otherwise the client runs a node of its own, serving the same API on the
//! same address, so everything after start-up is one code path: HTTP to
//! 127.0.0.1:8740.

use anyhow::{Result, bail};
use elemesh::daemon;
use serde::Deserialize;
use std::time::Duration;

pub const API: &str = "http://127.0.0.1:8740";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Starting,
    /// An elemesh daemon on this machine; the client only shows it.
    Daemon,
    /// The client's own node (scouting until it has the passphrase).
    Builtin,
}

#[derive(Deserialize, Default, Clone)]
pub struct Status {
    pub node: String,
    /// False while the node has no passphrase and only listens.
    #[serde(default = "yes")]
    pub joined: bool,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub links: Vec<Link>,
    #[serde(default)]
    pub nearby: Vec<Nearby>,
    #[serde(default)]
    pub servers: Vec<Server>,
}

#[derive(Deserialize, Clone)]
pub struct Link {
    pub peer: String,
    pub transport: String,
    pub via: String,
    pub heard_s: u64,
    #[serde(default)]
    pub name: Option<String>,
}

/// An ELE server the node found on the local network.
#[derive(Deserialize, Clone)]
pub struct Server {
    pub name: String,
    #[serde(default)]
    pub host: String,
    pub url: String,
    pub addrs: Vec<String>,
}

#[derive(Deserialize, Clone)]
pub struct Nearby {
    pub peer: Option<String>,
    pub transport: String,
    pub via: String,
    pub seen_s: u64,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Deserialize, Clone)]
pub struct Message {
    pub seq: u64,
    #[serde(default)]
    pub control: Option<Control>,
    pub author: String,
    pub ts_ms: u64,
    pub nick: String,
    pub text: String,
}

/// A delete or clear riding in the room; applied to the view, never shown.
#[derive(Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    Delete { author: String, seq: u64 },
    Clear,
}

fn yes() -> bool {
    true
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

fn send_json(method: &str, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
    let a = agent(Duration::from_secs(5));
    let url = format!("{API}{path}");
    let mut r = match method {
        "PUT" => a.put(&url).send_json(body)?,
        "DELETE" => a.delete(&url).call()?,
        _ => a.post(&url).send_json(body)?,
    };
    let v: serde_json::Value = r.body_mut().read_json().unwrap_or_default();
    if r.status() != 200 {
        bail!("{}", v["error"].as_str().unwrap_or("not accepted"));
    }
    Ok(v)
}

/// Deletes one of this machine's own messages, on every machine.
pub fn delete(room: &str, seq: u64) -> Result<()> {
    send_json("DELETE", &format!("/v1/rooms/{}/messages/{seq}", url_part(room)), serde_json::Value::Null).map(|_| ())
}

/// Clears a room for everyone on the mesh, or only on this machine.
pub fn clear(room: &str, everyone: bool) -> Result<()> {
    send_json("POST", &format!("/v1/rooms/{}/clear", url_part(room)), serde_json::json!({ "everyone": everyone })).map(|_| ())
}

/// Renames this machine; empty goes back to its host name.
pub fn set_name(name: &str) -> Result<String> {
    let v = send_json("PUT", "/v1/name", serde_json::json!({ "name": name }))?;
    Ok(v["name"].as_str().unwrap_or_default().to_string())
}

/// Joins the mesh or changes its passphrase, then waits for the node to come
/// back with it (the service manager or the app restarts it).
pub fn set_passphrase(passphrase: &str) -> Result<()> {
    send_json("POST", "/v1/passphrase", serde_json::json!({ "passphrase": passphrase }))?;
    std::thread::sleep(Duration::from_secs(1));
    for _ in 0..40 {
        if status().map(|s| s.joined).unwrap_or(false) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("the passphrase is saved, but the mesh service hasn't come back yet")
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
    stop: std::sync::Arc<tokio::sync::Notify>,
    stopping: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl Embedded {
    /// Runs the node the way the service does (scouting until there is a
    /// passphrase) and starts it again whenever it asks, e.g. after joining.
    /// Each run gets a fresh runtime, so its sockets close before the next.
    fn start() -> Result<Self> {
        use std::sync::atomic::Ordering;
        let opts = daemon::Options::new(daemon::state_dir(None)?);
        let stop = std::sync::Arc::new(tokio::sync::Notify::new());
        let stopping = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (stop_t, stopping_t) = (stop.clone(), stopping.clone());
        let thread = std::thread::Builder::new().name("ele-node".into()).spawn(move || {
            while !stopping_t.load(Ordering::SeqCst) {
                let rt = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => return tracing::error!("node runtime: {e}"),
                };
                let (opts, stop) = (opts.clone(), stop_t.clone());
                let result = rt.block_on(async move {
                    tokio::select! {
                        r = daemon::run(opts) => Some(r),
                        _ = stop.notified() => None,
                    }
                });
                rt.shutdown_timeout(Duration::from_secs(2));
                match result {
                    None => break,
                    Some(Ok(())) => std::thread::sleep(Duration::from_millis(300)),
                    Some(Err(e)) => {
                        tracing::error!("node stopped: {e:#}");
                        std::thread::sleep(Duration::from_secs(5));
                    }
                }
            }
        })?;
        Ok(Self { stop, stopping, thread })
    }

    fn stop(self) {
        self.stopping.store(true, std::sync::atomic::Ordering::SeqCst);
        self.stop.notify_one();
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
        match Embedded::start() {
            Ok(e) => {
                self.embedded = Some(e);
                self.mode = Mode::Builtin;
            }
            Err(e) => self.error = Some(format!("could not start a node: {e:#}")),
        }
    }

    pub fn shutdown(&mut self) {
        if let Some(e) = self.embedded.take() {
            e.stop();
        }
    }
}
