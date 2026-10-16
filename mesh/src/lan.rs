//! Wi-Fi / Ethernet transport. Works on a router-less ad-hoc network: peers
//! find each other by UDP broadcast on every IPv4 interface and link over TCP.

use anyhow::Result;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::link;
use crate::node::Node;
use crate::proto::NodeId;

const MAGIC: &[u8; 4] = b"ELEM";

/// Stops the listener and beacon when `run` returns, so a retry can rebind.
struct TaskGuard(Vec<tokio::task::JoinHandle<()>>);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        for t in &self.0 {
            t.abort();
        }
    }
}
const BEACON_EVERY: Duration = Duration::from_secs(5);

fn beacon(node: &Node, tcp_port: u16) -> Vec<u8> {
    let mut b = MAGIC.to_vec();
    b.push(crate::proto::PROTOCOL_VERSION);
    b.extend_from_slice(&node.identity.id);
    b.extend_from_slice(&tcp_port.to_be_bytes());
    b
}

fn parse_beacon(b: &[u8]) -> Option<(NodeId, u16)> {
    if b.len() != 15 || &b[..4] != MAGIC {
        return None;
    }
    let id: NodeId = b[5..13].try_into().ok()?;
    Some((id, u16::from_be_bytes([b[13], b[14]])))
}

fn broadcast_targets(port: u16) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => v4.broadcast,
            _ => None,
        })
        .map(|b| SocketAddr::V4(SocketAddrV4::new(b, port)))
        .collect();
    out.push(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, port)));
    out.sort();
    out.dedup();
    out
}

pub async fn run(node: Arc<Node>, port: u16) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await?;
    let udp = Arc::new(UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).await?);
    udp.set_broadcast(true)?;
    tracing::info!("lan: listening on tcp/udp {port}");

    let mut tasks = TaskGuard(Vec::new());
    let n = node.clone();
    tasks.0.push(tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let _ = stream.set_nodelay(true);
                    let n = n.clone();
                    tokio::spawn(async move {
                        if let Err(e) = link::run(stream, false, "lan", addr.to_string(), n).await {
                            tracing::debug!("lan inbound {addr}: {e:#}");
                        }
                    });
                }
                Err(e) => tracing::warn!("lan accept: {e}"),
            }
        }
    }));

    // A scout only listens: announcing itself would invite dials it can't answer.
    if !node.scout {
        let sender = udp.clone();
        let b = beacon(&node, port);
        tasks.0.push(tokio::spawn(async move {
            loop {
                for target in broadcast_targets(port) {
                    let _ = sender.send_to(&b, target).await;
                }
                tokio::time::sleep(BEACON_EVERY).await;
            }
        }));
    }

    let mut buf = [0u8; 64];
    loop {
        let (len, from) = udp.recv_from(&mut buf).await?;
        let Some((peer, peer_port)) = parse_beacon(&buf[..len]) else { continue };
        node.heard(Some(peer), "lan", from.ip().to_string());
        // The lower id dials so two nodes don't race to connect to each other.
        if peer == node.identity.id || node.identity.id > peer || node.is_linked(&peer, "lan") {
            continue;
        }
        let addr = SocketAddr::new(from.ip(), peer_port);
        if !node.try_begin_dial(peer, "lan") {
            continue;
        }
        let n = node.clone();
        tokio::spawn(async move {
            let result = async {
                let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await??;
                stream.set_nodelay(true)?;
                link::run(stream, true, "lan", addr.to_string(), n.clone()).await
            }
            .await;
            if let Err(e) = &result {
                tracing::debug!("lan dial {addr}: {e:#}");
            }
            n.end_dial(peer, "lan", result.is_ok());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beacon_roundtrip() {
        let id = crate::identity::Identity::generate().unwrap();
        let mut b = MAGIC.to_vec();
        b.push(1);
        b.extend_from_slice(&id.id);
        b.extend_from_slice(&47474u16.to_be_bytes());
        assert_eq!(parse_beacon(&b), Some((id.id, 47474)));
        assert_eq!(parse_beacon(&b[..14]), None);
    }
}
