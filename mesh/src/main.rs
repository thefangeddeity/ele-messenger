mod api;
mod ble;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod ble_central;
#[cfg(target_os = "linux")]
mod ble_linux;
mod identity;
mod lan;
mod link;
mod node;
mod proto;
mod store;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DEFAULT_API: &str = "127.0.0.1:8740";
const DEFAULT_LAN_PORT: u16 = 47474;

#[derive(Parser)]
#[command(name = "elemesh", version, about = "ELE Messenger mesh daemon: chat that works without infrastructure")]
struct Cli {
    /// Where identity, passphrase and messages live
    #[arg(long, env = "ELEMESH_STATE_DIR", global = true)]
    state_dir: Option<PathBuf>,
    /// Local API address (loopback only)
    #[arg(long, env = "ELEMESH_API", default_value = DEFAULT_API, global = true)]
    api: SocketAddr,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set the mesh passphrase; only nodes that share it can link
    Init {
        #[arg(long, env = "ELEMESH_PASSPHRASE")]
        passphrase: String,
    },
    /// Run the daemon
    Run {
        #[arg(long, env = "ELEMESH_LAN_PORT", default_value_t = DEFAULT_LAN_PORT)]
        lan_port: u16,
        #[arg(long)]
        no_lan: bool,
        #[arg(long)]
        no_ble: bool,
    },
    /// Post a message through the local daemon
    Send {
        room: String,
        text: String,
        #[arg(long, default_value = "")]
        nick: String,
    },
    /// Print a room's messages and follow new ones
    Tail { room: String },
    /// Show this node and its links
    Status,
}

fn state_dir(cli: &Option<PathBuf>) -> Result<PathBuf> {
    let dir = match cli {
        Some(d) => d.clone(),
        None => dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .context("no state directory; pass --state-dir")?
            .join("elemesh"),
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn read_passphrase(dir: &std::path::Path) -> Result<String> {
    if let Ok(p) = std::env::var("ELEMESH_PASSPHRASE") {
        return Ok(p);
    }
    let path = dir.join("passphrase");
    let p = std::fs::read_to_string(&path)
        .with_context(|| format!("no mesh passphrase: run `elemesh init --passphrase …` (looked in {})", path.display()))?;
    Ok(p.trim_end_matches(['\r', '\n']).to_string())
}

async fn http(api: SocketAddr, method: &str, path: &str, body: Option<String>) -> Result<serde_json::Value> {
    let mut s = tokio::net::TcpStream::connect(api).await.with_context(|| format!("daemon not running at {api}"))?;
    let body = body.unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {api}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).await?;
    let text = String::from_utf8_lossy(&resp);
    let (head, rest) = text.split_once("\r\n\r\n").context("bad response")?;
    let status: u16 = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let v: serde_json::Value = serde_json::from_str(rest).unwrap_or(serde_json::Value::Null);
    if status != 200 {
        bail!("{status}: {}", v.get("error").and_then(|e| e.as_str()).unwrap_or(rest));
    }
    Ok(v)
}

fn url_part(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Keeps a transport running: restarts it 10 s after it fails, backing off
/// to once a minute while it keeps failing the same way (no adapter on this
/// machine), and warns once per distinct error rather than every retry.
fn supervise<F, Fut>(name: &'static str, start: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send,
{
    use std::time::Duration;
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
fn spawn_ble(node: Arc<node::Node>) {
    supervise("ble", move || ble_linux::run(node.clone()));
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn spawn_ble(node: Arc<node::Node>) {
    supervise("ble", move || ble_central::run(node.clone()));
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn spawn_ble(_node: Arc<node::Node>) {
    tracing::warn!("ble: not supported on this platform; using LAN only");
}

#[tokio::main]
async fn main() -> Result<()> {
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        .with_ansi(std::io::stdout().is_terminal())
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "elemesh=info".into()))
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init { passphrase } => {
            identity::derive_psk(&passphrase)?;
            let dir = state_dir(&cli.state_dir)?;
            identity::write_private(&dir.join("passphrase"), passphrase.as_bytes())?;
            let me = identity::Identity::load_or_create(&dir)?;
            println!("node {} ready in {}", hex::encode(me.id), dir.display());
        }
        Cmd::Run { lan_port, no_lan, no_ble } => {
            let dir = state_dir(&cli.state_dir)?;
            let psk = identity::derive_psk(&read_passphrase(&dir)?)?;
            let me = identity::Identity::load_or_create(&dir)?;
            let store = store::Store::open(&dir.join("messages.db"))?;
            let node = Arc::new(node::Node::new(me, psk, store));
            tracing::info!("node {} state {}", node.id_hex(), dir.display());
            let n = node.clone();
            tokio::spawn(async move { n.maintenance().await });
            if !no_lan {
                let n = node.clone();
                supervise("lan", move || lan::run(n.clone(), lan_port));
            }
            if !no_ble {
                spawn_ble(node.clone());
            }
            api::serve(node, cli.api).await?;
        }
        Cmd::Send { room, text, nick } => {
            let body = serde_json::json!({ "nick": nick, "text": text }).to_string();
            http(cli.api, "POST", &format!("/v1/rooms/{}/messages", url_part(&room)), Some(body)).await?;
        }
        Cmd::Tail { room } => {
            let mut after = 0i64;
            loop {
                let v = http(cli.api, "GET", &format!("/v1/rooms/{}/messages?after={after}&wait=25", url_part(&room)), None).await?;
                for m in v["messages"].as_array().into_iter().flatten() {
                    let author = m["author"].as_str().unwrap_or("?");
                    let nick = m["nick"].as_str().filter(|n| !n.is_empty()).unwrap_or(author.get(..6).unwrap_or(author));
                    println!("[{}] {}: {}", m["ts_ms"], nick, m["text"].as_str().unwrap_or(""));
                }
                after = v["cursor"].as_i64().unwrap_or(after);
            }
        }
        Cmd::Status => {
            println!("{}", serde_json::to_string_pretty(&http(cli.api, "GET", "/v1/status", None).await?)?);
        }
    }
    Ok(())
}
