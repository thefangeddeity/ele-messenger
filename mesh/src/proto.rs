use serde::{Deserialize, Serialize};

use crate::store::Entry;

pub type NodeId = [u8; 8];

pub const PROTOCOL_VERSION: u8 = 1;
pub const DEFAULT_TTL: u8 = 7;
/// Noise transport messages are capped at 65535 bytes; leave room for the tag.
pub const MAX_PLAINTEXT: usize = 60_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorState {
    pub author: NodeId,
    /// Everything at or below this is gone for the sender.
    pub base: u64,
    /// Highest seq held with no gap above `base`.
    pub high: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Wire {
    Hello { version: u8 },
    Summary(Vec<AuthorState>),
    Entries { ttl: u8, entries: Vec<Entry> },
    /// The sender's machine name, sent once a link is up. Last, so nodes that
    /// predate it only drop this one frame.
    Name(String),
    /// Signatures for the entries in the next Entries frame on this link.
    /// After Name for the same reason: an older node drops only this frame
    /// and takes the entries unsigned.
    Sigs(Vec<Signed>),
}

/// An entry's signature, with its author's identity key so any node can
/// check it: the key must hash to the author's id.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signed {
    pub author: NodeId,
    pub seq: u64,
    pub key: [u8; 32],
    /// 64 bytes (serde has no 64-byte arrays); anything else fails to verify.
    pub sig: Vec<u8>,
}

impl Wire {
    pub fn encode(&self) -> anyhow::Result<Vec<u8>> {
        Ok(postcard::to_stdvec(self)?)
    }

    pub fn decode(b: &[u8]) -> anyhow::Result<Self> {
        Ok(postcard::from_bytes(b)?)
    }
}
