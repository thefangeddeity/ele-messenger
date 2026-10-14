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
}

impl Wire {
    pub fn encode(&self) -> anyhow::Result<Vec<u8>> {
        Ok(postcard::to_stdvec(self)?)
    }

    pub fn decode(b: &[u8]) -> anyhow::Result<Self> {
        Ok(postcard::from_bytes(b)?)
    }
}
