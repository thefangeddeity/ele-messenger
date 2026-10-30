use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::proto::{AuthorState, NodeId};

pub const MAX_TEXT: usize = 2048;
pub const MAX_ROOM: usize = 64;
pub const MAX_NICK: usize = 64;

/// A control entry's text starts with this. It travels like any message, so
/// deletes and clears reach every node by the same sync and relaying; nodes
/// that predate it show it as odd text until they update.
pub const CONTROL_PREFIX: &str = "\u{1}ele:";

/// What a control entry asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    /// Remove one message. Honoured only when the control's author wrote it.
    Delete { author: String, seq: u64 },
    /// Hide everything in the room up to and including this entry's time.
    Clear,
}

impl Control {
    pub fn text(&self) -> String {
        format!("{CONTROL_PREFIX}{}", serde_json::to_string(self).unwrap_or_default())
    }

    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_str(text.strip_prefix(CONTROL_PREFIX)?).ok()
    }
}

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
    /// Set on control entries, which clients apply rather than show.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<Control>,
    /// Carries a valid signature from its author.
    pub signed: bool,
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
             );
             CREATE TABLE IF NOT EXISTS deleted (
                 author BLOB NOT NULL,
                 seq INTEGER NOT NULL,
                 PRIMARY KEY (author, seq)
             );
             CREATE TABLE IF NOT EXISTS cleared (
                 room TEXT PRIMARY KEY,
                 before_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS sigs (
                 author BLOB NOT NULL,
                 seq INTEGER NOT NULL,
                 key BLOB NOT NULL,
                 sig BLOB NOT NULL,
                 PRIMARY KEY (author, seq)
             );
             CREATE TABLE IF NOT EXISTS signers (
                 author BLOB PRIMARY KEY,
                 first_seq INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS cleared_here (
                 room TEXT PRIMARY KEY,
                 before_ms INTEGER NOT NULL
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
        if n == 1 {
            self.apply(e)?;
        }
        Ok(n == 1)
    }

    /// Records what a control entry asks for. Order doesn't matter: a delete
    /// that arrives before its message still hides it when it comes.
    fn apply(&self, e: &Entry) -> Result<()> {
        match Control::parse(&e.text) {
            Some(Control::Delete { author, seq }) if author == hex::encode(e.author) => {
                self.conn.execute(
                    "INSERT OR IGNORE INTO deleted (author, seq) VALUES (?1, ?2)",
                    params![&e.author[..], seq as i64],
                )?;
            }
            Some(Control::Clear) => self.raise_clear("cleared", &e.room, e.ts_ms)?,
            _ => {}
        }
        Ok(())
    }

    fn raise_clear(&self, table: &str, room: &str, before_ms: u64) -> Result<()> {
        self.conn.execute(
            &format!(
                "INSERT INTO {table} (room, before_ms) VALUES (?1, ?2)
                 ON CONFLICT(room) DO UPDATE SET before_ms = MAX(before_ms, excluded.before_ms)"
            ),
            params![room, before_ms as i64],
        )?;
        Ok(())
    }

    /// Hides the room's messages up to `before_ms` on this node only.
    pub fn clear_here(&self, room: &str, before_ms: u64) -> Result<()> {
        self.raise_clear("cleared_here", room, before_ms)
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

    /// Messages after `after`, minus deleted and cleared ones. Control
    /// entries are included so a client following the room can apply them.
    pub fn messages(&self, room: &str, after: i64, limit: u32) -> Result<Vec<StoredEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.rowid, e.author, e.seq, e.ts_ms, e.room, e.nick, e.text,
                    EXISTS (SELECT 1 FROM sigs s WHERE s.author = e.author AND s.seq = e.seq) FROM entries e
             WHERE e.room = ?1 AND e.rowid > ?2
               AND NOT EXISTS (SELECT 1 FROM deleted d WHERE d.author = e.author AND d.seq = e.seq)
               AND (instr(e.text, ?4) = 1 OR NOT EXISTS (
                     SELECT 1 FROM cleared c WHERE c.room = e.room AND e.ts_ms <= c.before_ms
                     UNION ALL
                     SELECT 1 FROM cleared_here h WHERE h.room = e.room AND e.ts_ms <= h.before_ms))
             ORDER BY e.rowid LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![room, after, limit, CONTROL_PREFIX], |r| {
            let a: Vec<u8> = r.get(1)?;
            let text: String = r.get(6)?;
            Ok(StoredEntry {
                cursor: r.get(0)?,
                author: hex::encode(a),
                seq: r.get::<_, i64>(2)? as u64,
                ts_ms: r.get::<_, i64>(3)? as u64,
                room: r.get(4)?,
                nick: r.get(5)?,
                control: Control::parse(&text),
                text,
                signed: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Keeps a verified signature, and remembers that its author signs from
    /// this seq on: later unsigned entries in its name are forgeries.
    pub fn put_sig(&self, s: &crate::proto::Signed) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO sigs (author, seq, key, sig) VALUES (?1, ?2, ?3, ?4)",
            params![&s.author[..], s.seq as i64, &s.key[..], &s.sig[..]],
        )?;
        self.conn.execute(
            "INSERT INTO signers (author, first_seq) VALUES (?1, ?2)
             ON CONFLICT(author) DO UPDATE SET first_seq = MIN(first_seq, excluded.first_seq)",
            params![&s.author[..], s.seq as i64],
        )?;
        Ok(())
    }

    /// The first seq from which `author` is known to sign.
    pub fn signs_from(&self, author: &crate::proto::NodeId) -> Result<Option<u64>> {
        Ok(self
            .conn
            .query_row("SELECT first_seq FROM signers WHERE author = ?1", params![&author[..]], |r| r.get::<_, i64>(0))
            .optional()?
            .map(|v| v as u64))
    }

    /// Stored signatures for these entries, to send alongside them.
    pub fn sigs_for(&self, entries: &[Entry]) -> Result<Vec<crate::proto::Signed>> {
        let mut stmt = self.conn.prepare_cached("SELECT key, sig FROM sigs WHERE author = ?1 AND seq = ?2")?;
        let mut out = Vec::new();
        for e in entries {
            let row: Option<(Vec<u8>, Vec<u8>)> =
                stmt.query_row(params![&e.author[..], e.seq as i64], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            if let Some((k, s)) = row {
                if let Ok(key) = k.try_into() {
                    out.push(crate::proto::Signed { author: e.author, seq: e.seq, key, sig: s });
                }
            }
        }
        Ok(out)
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
