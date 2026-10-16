use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::proto::{AuthorState, NodeId};

pub const MAX_TEXT: usize = 2048;
pub const MAX_ROOM: usize = 64;
pub const MAX_NICK: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub author: NodeId,
    pub seq: u64,
    pub ts_ms: u64,
    pub room: String,
    pub nick: String,
    pub text: String,
}

impl Entry {
    pub fn is_well_formed(&self) -> bool {
        self.seq > 0
            && !self.room.is_empty()
            && self.room.len() <= MAX_ROOM
            && self.nick.len() <= MAX_NICK
            && self.text.len() <= MAX_TEXT
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct StoredEntry {
    pub cursor: i64,
    pub author: String,
    pub seq: u64,
    pub ts_ms: u64,
    pub room: String,
    pub nick: String,
    pub text: String,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS entries (
                 author BLOB NOT NULL,
                 seq INTEGER NOT NULL,
                 ts_ms INTEGER NOT NULL,
                 room TEXT NOT NULL,
                 nick TEXT NOT NULL,
                 text TEXT NOT NULL,
                 received_ms INTEGER NOT NULL,
                 PRIMARY KEY (author, seq)
             );
             CREATE INDEX IF NOT EXISTS entries_room ON entries(room);
             CREATE TABLE IF NOT EXISTS bases (
                 author BLOB PRIMARY KEY,
                 base INTEGER NOT NULL
             );",
        )?;
        Ok(Self { conn })
    }

    /// Returns true when the entry was new.
    pub fn insert(&self, e: &Entry, received_ms: u64) -> Result<bool> {
        if e.seq <= self.base(&e.author)? {
            return Ok(false);
        }
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO entries (author, seq, ts_ms, room, nick, text, received_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![&e.author[..], e.seq as i64, e.ts_ms as i64, e.room, e.nick, e.text, received_ms as i64],
        )?;
        Ok(n == 1)
    }

    pub fn next_seq(&self, author: &NodeId) -> Result<u64> {
        let max: Option<i64> = self.conn.query_row(
            "SELECT MAX(seq) FROM entries WHERE author = ?1",
            params![&author[..]],
            |r| r.get(0),
        )?;
        Ok(max.unwrap_or(0).max(self.base(author)? as i64) as u64 + 1)
    }

    fn base(&self, author: &NodeId) -> Result<u64> {
        let b: Option<i64> = self
            .conn
            .query_row("SELECT base FROM bases WHERE author = ?1", params![&author[..]], |r| r.get(0))
            .optional()?;
        Ok(b.unwrap_or(0) as u64)
    }

    fn set_base(&self, author: &NodeId, base: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bases (author, base) VALUES (?1, ?2)
             ON CONFLICT(author) DO UPDATE SET base = MAX(base, excluded.base)",
            params![&author[..], base as i64],
        )?;
        self.conn.execute(
            "DELETE FROM entries WHERE author = ?1 AND seq <= ?2",
            params![&author[..], base as i64],
        )?;
        Ok(())
    }

    /// Highest seq held with no gap above the author's base.
    fn high(&self, author: &NodeId, base: u64) -> Result<u64> {
        let first_gap: Option<i64> = self
            .conn
            .query_row(
                "SELECT s FROM (
                     SELECT ?2 AS s
                     UNION ALL
                     SELECT seq FROM entries WHERE author = ?1 AND seq > ?2
                 ) WHERE s + 1 NOT IN (SELECT seq FROM entries WHERE author = ?1)
                 ORDER BY s LIMIT 1",
                params![&author[..], base as i64],
                |r| r.get(0),
            )
            .optional()?;
        Ok(first_gap.map(|s| s as u64).unwrap_or(base))
    }

    pub fn summary(&self) -> Result<Vec<AuthorState>> {
        let mut stmt = self.conn.prepare(
            "SELECT author FROM entries UNION SELECT author FROM bases",
        )?;
        let authors: Vec<Vec<u8>> = stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let mut out = Vec::with_capacity(authors.len());
        for a in authors {
            let Ok(author) = <NodeId>::try_from(a.as_slice()) else { continue };
            let base = self.base(&author)?;
            out.push(AuthorState { author, base, high: self.high(&author, base)? });
        }
        Ok(out)
    }

    /// Entries this node holds that a peer with `theirs` lacks. Also adopts a
    /// peer's base when everything below it is gone for both of us.
    pub fn missing_for(&self, theirs: &[AuthorState], limit_bytes: usize) -> Result<Vec<Entry>> {
        for s in theirs {
            let base = self.base(&s.author)?;
            if s.base > self.high(&s.author, base)? {
                self.set_base(&s.author, s.base)?;
            }
        }
        let mut stmt = self.conn.prepare(
            "SELECT author, seq, ts_ms, room, nick, text FROM entries ORDER BY received_ms, author, seq",
        )?;
        let rows = stmt.query_map([], |r| {
            let a: Vec<u8> = r.get(0)?;
            Ok((a, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?;
        let mut out = Vec::new();
        let mut size = 0usize;
        for row in rows {
            let (a, seq, ts, room, nick, text): (Vec<u8>, i64, i64, String, String, String) = row?;
            let Ok(author) = <NodeId>::try_from(a.as_slice()) else { continue };
            let seq = seq as u64;
            let wanted = match theirs.iter().find(|s| s.author == author) {
                Some(s) => seq > s.high,
                None => true,
            };
            if !wanted {
                continue;
            }
            size += 32 + room.len() + nick.len() + text.len();
            if size > limit_bytes && !out.is_empty() {
                break;
            }
            out.push(Entry { author, seq, ts_ms: ts as u64, room, nick, text });
        }
        Ok(out)
    }

    pub fn messages(&self, room: &str, after: i64, limit: u32) -> Result<Vec<StoredEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT rowid, author, seq, ts_ms, room, nick, text FROM entries
             WHERE room = ?1 AND rowid > ?2 ORDER BY rowid LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![room, after, limit], |r| {
            let a: Vec<u8> = r.get(1)?;
            Ok(StoredEntry {
                cursor: r.get(0)?,
                author: hex::encode(a),
                seq: r.get::<_, i64>(2)? as u64,
                ts_ms: r.get::<_, i64>(3)? as u64,
                room: r.get(4)?,
                nick: r.get(5)?,
                text: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn rooms(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT room FROM entries ORDER BY room")?;
        Ok(stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)
    }

    pub fn count(&self) -> Result<u64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get::<_, i64>(0))? as u64)
    }

    /// Drops entries received before `cutoff_ms` and advances bases past them.
    pub fn prune(&self, cutoff_ms: u64) -> Result<u64> {
        let mut stmt = self.conn.prepare(
            "SELECT author, MAX(seq) FROM entries WHERE received_ms < ?1 GROUP BY author",
        )?;
        let gone: Vec<(Vec<u8>, i64)> = stmt
            .query_map(params![cutoff_ms as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let before = self.count()?;
        for (a, max) in gone {
            if let Ok(author) = <NodeId>::try_from(a.as_slice()) {
                self.set_base(&author, max as u64)?;
            }
        }
        Ok(before - self.count()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(author: u8, seq: u64) -> Entry {
        Entry { author: [author; 8], seq, ts_ms: seq, room: "r".into(), nick: "n".into(), text: format!("{author}-{seq}") }
    }

    fn state(s: &Store, author: u8) -> AuthorState {
        s.summary().unwrap().into_iter().find(|x| x.author == [author; 8]).unwrap()
    }

    #[test]
    fn high_stops_at_first_gap() {
        let s = Store::in_memory().unwrap();
        for q in [1, 2, 3, 5, 6] {
            s.insert(&e(1, q), 0).unwrap();
        }
        assert_eq!(state(&s, 1).high, 3);
        s.insert(&e(1, 4), 0).unwrap();
        assert_eq!(state(&s, 1).high, 6);
    }

    #[test]
    fn duplicate_insert_is_not_new() {
        let s = Store::in_memory().unwrap();
        assert!(s.insert(&e(1, 1), 0).unwrap());
        assert!(!s.insert(&e(1, 1), 0).unwrap());
    }

    #[test]
    fn missing_covers_other_authors_history() {
        // The case the earlier per-conversation counter lost.
        let a = Store::in_memory().unwrap();
        let b = Store::in_memory().unwrap();
        for q in 1..=5 { a.insert(&e(1, q), 0).unwrap(); }
        for q in 1..=3 { b.insert(&e(2, q), 0).unwrap(); }
        for x in a.missing_for(&b.summary().unwrap(), usize::MAX).unwrap() { b.insert(&x, 0).unwrap(); }
        for x in b.missing_for(&a.summary().unwrap(), usize::MAX).unwrap() { a.insert(&x, 0).unwrap(); }
        assert_eq!(a.count().unwrap(), 8);
        assert_eq!(b.count().unwrap(), 8);
        assert!(a.missing_for(&b.summary().unwrap(), usize::MAX).unwrap().is_empty());
    }

    #[test]
    fn pruned_history_stops_being_requested() {
        let a = Store::in_memory().unwrap();
        let b = Store::in_memory().unwrap();
        for q in 1..=4 { a.insert(&e(1, q), 0).unwrap(); }
        a.insert(&e(1, 5), 10).unwrap();
        a.prune(5).unwrap();
        assert_eq!(state(&a, 1).base, 4);
        // b only saw seq 5 live and has a gap below it.
        b.insert(&e(1, 5), 10).unwrap();
        assert_eq!(state(&b, 1).high, 0);
        // b learns a gave up on 1..4 and stops asking.
        b.missing_for(&a.summary().unwrap(), usize::MAX).unwrap();
        assert_eq!(state(&b, 1).high, 5);
        assert!(a.missing_for(&b.summary().unwrap(), usize::MAX).unwrap().is_empty());
    }

    #[test]
    fn next_seq_survives_pruning() {
        let s = Store::in_memory().unwrap();
        for q in 1..=3 { s.insert(&e(1, q), 0).unwrap(); }
        s.prune(1).unwrap();
        assert_eq!(s.count().unwrap(), 0);
        assert_eq!(s.next_seq(&[1; 8]).unwrap(), 4);
    }
}
