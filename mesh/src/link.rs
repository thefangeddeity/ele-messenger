use anyhow::{Context, Result, bail};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::identity::{self, NOISE_PARAMS};
use crate::node::{Node, SYNC_INTERVAL};
use crate::proto::{NodeId, Wire};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const NOISE_MAX: usize = 65535;
const IDLE_TIMEOUT: Duration = Duration::from_secs(3 * SYNC_INTERVAL.as_secs());

async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> Result<()> {
    let len = r.read_u16().await? as usize;
    buf.resize(len, 0);
    r.read_exact(buf).await?;
    Ok(())
}

async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> Result<()> {
    let mut out = Vec::with_capacity(2 + data.len());
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
    w.write_all(&out).await?;
    w.flush().await?;
    Ok(())
}

async fn handshake<R, W>(
    r: &mut R,
    w: &mut W,
    initiator: bool,
    node: &Node,
) -> Result<(NodeId, snow::TransportState)>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let b = snow::Builder::new(NOISE_PARAMS.parse()?)
        .local_private_key(&node.identity.private)?
        .psk(0, &node.psk)?;
    let mut hs = if initiator { b.build_initiator()? } else { b.build_responder()? };
    let mut msg = vec![0u8; NOISE_MAX];
    let mut payload = vec![0u8; NOISE_MAX];
    let mut frame = Vec::new();
    // XXpsk0: -> psk, e ; <- e, ee, s, es ; -> s, se
    for step in 0..3 {
        let mine = (step % 2 == 0) == initiator;
        if mine {
            let n = hs.write_message(&[], &mut msg)?;
            write_frame(w, &msg[..n]).await?;
        } else {
            read_frame(r, &mut frame).await?;
            hs.read_message(&frame, &mut payload).context("handshake rejected (wrong mesh passphrase?)")?;
        }
    }
    let remote = hs.get_remote_static().context("peer sent no static key")?.to_vec();
    Ok((identity::node_id(&remote), hs.into_transport_mode()?))
}

/// Runs a link to completion over any duplex byte stream.
pub async fn run<S>(stream: S, initiator: bool, transport: &'static str, label: String, node: Arc<Node>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut r, mut w) = tokio::io::split(stream);
    let (peer, ts) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(&mut r, &mut w, initiator, &node))
        .await
        .context("handshake timed out")??;
    if peer == node.identity.id {
        bail!("connected to myself");
    }
    let ts = Arc::new(Mutex::new(ts));
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(256);
    let Some(link_id) = node.add_link(peer, initiator, transport, label.clone(), tx) else {
        tracing::debug!("{transport} link to {} redundant, closing", hex::encode(peer));
        return Ok(());
    };
    tracing::info!("{transport} link up: {} ({label})", hex::encode(peer));

    let wts = ts.clone();
    let writer = tokio::spawn(async move {
        let mut buf = vec![0u8; NOISE_MAX];
        while let Some(plain) = rx.recv().await {
            let n = wts.lock().unwrap().write_message(&plain, &mut buf)?;
            write_frame(&mut w, &buf[..n]).await?;
        }
        // Dropped from the link table (redundant or closing): tell the peer.
        let _ = w.shutdown().await;
        anyhow::Ok(())
    });

    let reader_node = node.clone();
    let reader = async move {
        let mut frame = Vec::new();
        let mut plain = vec![0u8; NOISE_MAX];
        loop {
            // Peers send a summary every SYNC_INTERVAL, so silence means the
            // other end is gone even if the transport never said so (a BLE
            // adapter reset leaves the far side holding a dead connection).
            tokio::time::timeout(IDLE_TIMEOUT, read_frame(&mut r, &mut frame))
                .await
                .context("peer silent")??;
            let n = ts.lock().unwrap().read_message(&frame, &mut plain)?;
            match Wire::decode(&plain[..n]) {
                Ok(msg) => reader_node.handle(link_id, msg),
                Err(e) => tracing::warn!("bad frame from {}: {e}", hex::encode(peer)),
            }
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    };

    let result = tokio::select! {
        r = reader => r,
        w = writer => w.map_err(anyhow::Error::from).and_then(|x| x),
    };
    node.remove_link(link_id);
    tracing::info!("{transport} link down: {} ({:?})", hex::encode(peer), result.as_ref().err());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Identity, derive_psk};
    use crate::store::Store;

    fn node(pass: &str) -> Arc<Node> {
        Arc::new(Node::new(Identity::generate().unwrap(), derive_psk(pass).unwrap(), Store::in_memory().unwrap()))
    }

    fn count(n: &Node) -> u64 {
        n.with_store(|s| s.count()).unwrap()
    }

    async fn settle(cond: impl Fn() -> bool) -> bool {
        for _ in 0..100 {
            if cond() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        false
    }

    #[tokio::test]
    async fn history_syncs_both_ways_on_link_up() {
        let (a, b) = (node("same passphrase"), node("same passphrase"));
        a.post("home", "a", "written while alone").unwrap();
        a.post("home", "a", "second").unwrap();
        b.post("home", "b", "from b").unwrap();
        let (sa, sb) = tokio::io::duplex(4096);
        tokio::spawn(run(sa, true, "test", "pipe".into(), a.clone()));
        tokio::spawn(run(sb, false, "test", "pipe".into(), b.clone()));
        assert!(settle(|| count(&a) == 3 && count(&b) == 3).await, "a={} b={}", count(&a), count(&b));
        // Live message after the link is up.
        b.post("home", "b", "live").unwrap();
        assert!(settle(|| count(&a) == 4).await);
    }

    #[tokio::test]
    async fn relays_through_a_middle_node() {
        let (a, m, c) = (node("same passphrase"), node("same passphrase"), node("same passphrase"));
        let (x1, x2) = tokio::io::duplex(4096);
        let (y1, y2) = tokio::io::duplex(4096);
        tokio::spawn(run(x1, true, "test", "a-m".into(), a.clone()));
        tokio::spawn(run(x2, false, "test", "a-m".into(), m.clone()));
        tokio::spawn(run(y1, true, "test", "m-c".into(), m.clone()));
        tokio::spawn(run(y2, false, "test", "m-c".into(), c.clone()));
        assert!(settle(|| a.links().len() == 1 && c.links().len() == 1).await);
        a.post("home", "a", "two hops").unwrap();
        assert!(settle(|| count(&c) == 1).await, "flood did not reach c");
    }

    #[tokio::test]
    async fn wrong_passphrase_cannot_link() {
        let (a, b) = (node("our household"), node("a stranger's mesh"));
        a.post("home", "a", "private").unwrap();
        let (sa, sb) = tokio::io::duplex(4096);
        let ra = tokio::spawn(run(sa, true, "test", "pipe".into(), a.clone()));
        let rb = tokio::spawn(run(sb, false, "test", "pipe".into(), b.clone()));
        let (ra, rb) = (ra.await.unwrap(), rb.await.unwrap());
        assert!(ra.is_err() || rb.is_err());
        assert_eq!(count(&b), 0);
        assert!(a.links().is_empty() && b.links().is_empty());
    }
}
