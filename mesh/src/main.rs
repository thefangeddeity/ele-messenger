use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use elemesh::daemon::{self, DEFAULT_API, DEFAULT_LAN_PORT};
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        /// Listen only and report nearby nodes; needs no passphrase
        #[arg(long)]
        scout: bool,
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
            let dir = daemon::state_dir(cli.state_dir)?;
            let id = daemon::init(&dir, &passphrase)?;
            println!("node {} ready in {}", hex::encode(id), dir.display());
        }
        Cmd::Run { lan_port, no_lan, no_ble, scout } => {
            let opts = daemon::Options {
                state_dir: daemon::state_dir(cli.state_dir)?,
                api: cli.api,
                lan_port,
                lan: !no_lan,
                ble: !no_ble,
            };
            if scout { daemon::scout(opts).await? } else { daemon::run(opts).await? }
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
