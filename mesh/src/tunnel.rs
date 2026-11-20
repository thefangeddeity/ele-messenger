//! SSH over the mesh. A program on this machine (`elemesh ssh-proxy`, used as
//! ssh's ProxyCommand) connects to the tunnel port, names a machine, and gets
//! a byte stream to that machine's own sshd, carried inside the encrypted
//! link. Off by default: a machine answers only once its owner turns it on,
//! and ssh's own keys or password still decide who may log in.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use crate::node::Node;
use crate::proto::{TunnelOp, Wire};

pub const DEFAULT_PORT: u16 = 8741;
const SSHD: &str = "127.0.0.1:22";
/// Data per frame: small enough for BLE links to interleave chat with it.
const CHUNK: usize = 16 * 1024;

/// One end of a tunnel: (link, channel, whether this node opened it).
type Key = (u64, u32, bool);

#[derive(Default)]
pub struct Tunnels {
    pub allowed: AtomicBool,
    /// Where an accepted tunnel goes; tests point it at their own server.
    pub target: Mutex<Option<String>>,
    next: AtomicU32,
    open: Mutex<HashMap<Key, mpsc::UnboundedSender<Vec<u8>>>>,
    waiting: Mutex<HashMap<Key, oneshot::Sender<std::result::Result<(), String>>>>,
}

impl Tunnels {
    pub fn drop_link(&self, link: u64) {
        self.open.lock().unwrap().retain(|k, _| k.0 != link);
        self.waiting.lock().unwrap().retain(|k, _| k.0 != link);
    }
}

/// Reads whether this machine accepts SSH over the mesh.
pub fn load_allowed(dir: &std::path::Path) -> bool {
    std::fs::read_to_string(dir.join("ssh")).is_ok_and(|s| s.trim() == "on")
}

pub fn save_allowed(dir: &std::path::Path, on: bool) -> Result<()> {
    std::fs::write(dir.join("ssh"), if on { "on\n" } else { "off\n" })?;
    Ok(())
}

/// A tunnel frame from a peer.
pub fn handle(node: &Arc<Node>, link: u64, ch: u32, from_opener: bool, op: TunnelOp) {
    let t = &node.tunnels;
    let key = (link, ch, !from_opener);
    match op {
        TunnelOp::Open => {
            if !t.allowed.load(Ordering::Relaxed) {
                node.send_frame(link, &tunnel(ch, false, TunnelOp::Close("SSH over the mesh is off on that machine".into())));
                return;
            }
            let node = node.clone();
            tokio::spawn(async move {
                let to = node.tunnels.target.lock().unwrap().clone().unwrap_or_else(|| SSHD.to_string());
                match TcpStream::connect(to).await {
                    Ok(s) => {
                        node.send_frame(link, &tunnel(ch, false, TunnelOp::Opened));
                        pump(node, key, s).await;
                    }
                    Err(e) => node.send_frame(link, &tunnel(ch, false, TunnelOp::Close(format!("no SSH server there: {e}")))),
                }
            });
        }
        TunnelOp::Opened => {
            if let Some(w) = t.waiting.lock().unwrap().remove(&key) {
                let _ = w.send(Ok(()));
            }
        }
        TunnelOp::Data(bytes) => {
            if let Some(tx) = t.open.lock().unwrap().get(&key) {
                let _ = tx.send(bytes);
            }
        }
        TunnelOp::Close(why) => {
            if let Some(w) = t.waiting.lock().unwrap().remove(&key) {
                let _ = w.send(Err(why));
            }
            t.open.lock().unwrap().remove(&key);
        }
    }
}

fn tunnel(ch: u32, from_opener: bool, op: TunnelOp) -> Wire {
    Wire::Tunnel { ch, from_opener, op }
}

/// Carries bytes both ways between a local socket and the peer, until
/// either side closes.
async fn pump(node: Arc<Node>, key: Key, s: TcpStream) {
    let (link, ch, mine) = key;
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    node.tunnels.open.lock().unwrap().insert(key, tx);
    let (mut r, mut w) = s.into_split();
    let down = async {
        while let Some(b) = rx.recv().await {
            if w.write_all(&b).await.is_err() {
                break;
            }
        }
    };
    let up = async {
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = match r.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if !node.send_frame_wait(link, &tunnel(ch, mine, TunnelOp::Data(buf[..n].to_vec()))).await {
                break;
            }
        }
    };
    tokio::select! { _ = down => {}, _ = up => {} }
    node.tunnels.open.lock().unwrap().remove(&key);
    node.send_frame(link, &tunnel(ch, mine, TunnelOp::Close(String::new())));
}

/// Listens for local programs asking for a tunnel. A client sends one line,
/// the machine's name or id, and reads back "OK" or "ERR <why>"; after OK
/// the connection is the stream.
pub async fn serve(node: Arc<Node>, addr: SocketAddr) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("tunnel: {addr}");
    loop {
        let (s, _) = listener.accept().await?;
        let node = node.clone();
        tokio::spawn(async move {
            if let Err(e) = client(node, s).await {
                tracing::debug!("tunnel: {e:#}");
            }
        });
    }
}

/// One line, read a byte at a time: a buffered reader would swallow the
/// stream's first bytes (ssh's banner) along with it.
async fn read_line(s: &mut TcpStream) -> Result<String> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while line.len() < 256 {
        if s.read(&mut b).await? == 0 || b[0] == b'\n' {
            break;
        }
        line.push(b[0]);
    }
    Ok(String::from_utf8_lossy(&line).trim().to_string())
}

async fn client(node: Arc<Node>, mut s: TcpStream) -> Result<()> {
    let want = read_line(&mut s).await?;
    let opened = open(&node, &want).await;
    let key = match opened {
        Ok(k) => k,
        Err(e) => {
            s.write_all(format!("ERR {e:#}\n").as_bytes()).await?;
            return Ok(());
        }
    };
    s.write_all(b"OK\n").await?;
    pump(node, key, s).await;
    Ok(())
}

async fn open(node: &Arc<Node>, want: &str) -> Result<Key> {
    let link = node.link_to(want).with_context(|| format!("no link to {want:?} right now"))?;
    let ch = node.tunnels.next.fetch_add(1, Ordering::Relaxed);
    let key = (link, ch, true);
    let (tx, rx) = oneshot::channel();
    node.tunnels.waiting.lock().unwrap().insert(key, tx);
    node.send_frame(link, &tunnel(ch, true, TunnelOp::Open));
    match tokio::time::timeout(std::time::Duration::from_secs(20), rx).await {
        Ok(Ok(Ok(()))) => Ok(key),
        Ok(Ok(Err(why))) => bail!("{why}"),
        _ => {
            node.tunnels.waiting.lock().unwrap().remove(&key);
            bail!("{want} didn't answer (it may run an older ELE)")
        }
    }
}

/// `elemesh ssh-proxy`: joins stdin/stdout to a tunnel, for ssh's ProxyCommand.
pub async fn proxy(port: u16, machine: &str) -> Result<()> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.context("the ELE mesh service isn't running here")?;
    s.write_all(format!("{machine}\n").as_bytes()).await?;
    let answer = read_line(&mut s).await?;
    match answer.strip_prefix("ERR ") {
        Some(why) => bail!("{why}"),
        None if answer == "OK" => {}
        None => bail!("unexpected answer from the mesh service"),
    }
    let (mut r, mut w) = s.into_split();
    let up = async { tokio::io::copy(&mut tokio::io::stdin(), &mut w).await };
    let down = async { tokio::io::copy(&mut r, &mut tokio::io::stdout()).await };
    tokio::select! { _ = up => {}, _ = down => {} }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{derive_psk, Identity};
    use crate::store::Store;

    fn node() -> Arc<Node> {
        Arc::new(Node::new(Identity::generate().unwrap(), derive_psk("same passphrase").unwrap(), Store::in_memory().unwrap()))
    }

    /// Asks `a`'s tunnel port for `want`; returns the answer line and the stream.
    async fn ask(port: SocketAddr, want: &str) -> (String, TcpStream) {
        let mut s = TcpStream::connect(port).await.unwrap();
        s.write_all(format!("{want}\n").as_bytes()).await.unwrap();
        (read_line(&mut s).await.unwrap(), s)
    }

    #[tokio::test]
    async fn a_tunnel_reaches_the_peers_server_only_when_allowed() {
        let (a, b) = (node(), node());
        let (x, y) = tokio::io::duplex(64 * 1024);
        tokio::spawn(crate::link::run(x, true, "test", "pipe".into(), a.clone()));
        tokio::spawn(crate::link::run(y, false, "test", "pipe".into(), b.clone()));
        for _ in 0..100 {
            if a.links().len() == 1 && b.links().len() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // b's "SSH server": echoes what it gets.
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        *b.tunnels.target.lock().unwrap() = Some(echo.local_addr().unwrap().to_string());
        tokio::spawn(async move {
            while let Ok((s, _)) = echo.accept().await {
                tokio::spawn(async move {
                    // Like sshd, it speaks first.
                    let (mut r, mut w) = s.into_split();
                    let _ = w.write_all(b"SSH-2.0-test\r\n").await;
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        let port_l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = port_l.local_addr().unwrap();
        let an = a.clone();
        tokio::spawn(async move {
            while let Ok((s, _)) = port_l.accept().await {
                tokio::spawn(client(an.clone(), s));
            }
        });
        let who = hex::encode(b.identity.id);

        let (answer, _) = ask(port, &who).await;
        assert!(answer.starts_with("ERR") && answer.contains("off"), "{answer}");
        let (answer, _) = ask(port, "nobody").await;
        assert!(answer.starts_with("ERR"), "{answer}");

        b.tunnels.allowed.store(true, Ordering::Relaxed);
        let (answer, mut s) = ask(port, &who).await;
        assert_eq!(answer, "OK");
        let big: Vec<u8> = (0..100_000u32).map(|i| i as u8).collect();
        let mut banner = [0u8; 14];
        s.read_exact(&mut banner).await.unwrap();
        assert_eq!(&banner, b"SSH-2.0-test\r\n", "the server's first bytes arrive whole");
        s.write_all(&big).await.unwrap();
        let mut back = vec![0u8; big.len()];
        s.read_exact(&mut back).await.unwrap();
        assert_eq!(back, big, "bytes cross intact, in order, across chunks");
    }
}
