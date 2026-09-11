//! Local storage. Every node's database is its own; nothing replicates.
//!
//! This is also the trust-on-first-use keyring. The job it does that matters
//! is noticing when a familiar nickname turns up carrying an unfamiliar key.

use crate::identity::PeerId;
use crate::peer::TrustState;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

const SCHEMA_VERSION: i64 = 1;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("could not open database at {}", path.display()))?;
        Self::from_connection(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        // WAL keeps the dashboard readable while the discovery loop writes.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let current: i64 = self.conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if current >= SCHEMA_VERSION {
            return Ok(());
        }
        self.conn
            .execute_batch(SCHEMA_V1)
            .context("could not apply the database schema")?;
        self.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    /// Record a sighting and report what we can honestly say about the peer.
    ///
    /// Called on every discovery packet, so it must stay cheap and idempotent.
    pub fn observe_peer(&self, peer_id: PeerId, nickname: &str, now: u64) -> Result<TrustState> {
        let hex = peer_id.to_hex();

        let existing: Option<i64> = self
            .conn
            .query_row("SELECT verified FROM known_peers WHERE peer_id = ?1", params![hex], |r| {
                r.get(0)
            })
            .optional()?;

        if let Some(verified) = existing {
            self.conn.execute(
                "UPDATE known_peers SET nickname = ?2, last_seen = ?3 WHERE peer_id = ?1",
                params![hex, nickname, now as i64],
            )?;
            return Ok(if verified != 0 { TrustState::Verified } else { TrustState::Known });
        }

        // A key we have never seen. If the name is already spoken for by a
        // different key, say so rather than quietly showing a familiar label.
        let impostor: Option<String> = self
            .conn
            .query_row(
                "SELECT peer_id FROM known_peers WHERE nickname = ?1 AND peer_id != ?2 LIMIT 1",
                params![nickname, hex],
                |r| r.get(0),
            )
            .optional()?;

        self.conn.execute(
            "INSERT INTO known_peers (peer_id, nickname, first_seen, last_seen, verified)
             VALUES (?1, ?2, ?3, ?3, 0)",
            params![hex, nickname, now as i64],
        )?;

        Ok(if impostor.is_some() { TrustState::NicknameConflict } else { TrustState::New })
    }

    /// Mark that a human compared fingerprints out of band and they matched.
    pub fn mark_verified(&self, peer_id: PeerId) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE known_peers SET verified = 1 WHERE peer_id = ?1",
            params![peer_id.to_hex()],
        )?;
        Ok(changed > 0)
    }

    pub fn known_peer_count(&self) -> Result<u64> {
        let count: i64 = self.conn.query_row("SELECT COUNT(*) FROM known_peers", [], |r| r.get(0))?;
        Ok(count as u64)
    }

    /// Remember a hub we announced ourselves to, so the UI can offer it again.
    pub fn record_hub_visit(&self, hub_id: PeerId, hub_name: &str, now: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO hub_visits (hub_id, hub_name, first_joined, last_joined)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(hub_id) DO UPDATE SET hub_name = ?2, last_joined = ?3",
            params![hub_id.to_hex(), hub_name, now as i64],
        )?;
        Ok(())
    }

    /// Hubs this vault has joined before, most recent first.
    pub fn hub_history(&self, limit: u32) -> Result<Vec<HubVisit>> {
        let mut stmt = self.conn.prepare(
            "SELECT hub_id, hub_name, first_joined, last_joined
             FROM hub_visits ORDER BY last_joined DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(HubVisit {
                hub_id: row.get::<_, String>(0)?,
                hub_name: row.get(1)?,
                first_joined: row.get::<_, i64>(2)? as u64,
                last_joined: row.get::<_, i64>(3)? as u64,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HubVisit {
    pub hub_id: String,
    pub hub_name: String,
    pub first_joined: u64,
    pub last_joined: u64,
}

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS known_peers (
    peer_id    TEXT    PRIMARY KEY,
    nickname   TEXT    NOT NULL,
    first_seen INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL,
    verified   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_known_peers_nickname ON known_peers (nickname);

CREATE TABLE IF NOT EXISTS hub_visits (
    hub_id       TEXT    PRIMARY KEY,
    hub_name     TEXT    NOT NULL,
    first_joined INTEGER NOT NULL,
    last_joined  INTEGER NOT NULL
);

-- Populated in P1. Mail is stored by the sender until the recipient's node is
-- reachable, then delivered directly. Hubs never hold it.
CREATE TABLE IF NOT EXISTS mail (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    direction    TEXT    NOT NULL CHECK (direction IN ('in', 'out')),
    peer_id      TEXT    NOT NULL,
    subject      TEXT    NOT NULL DEFAULT '',
    body         TEXT    NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    delivered_at INTEGER,
    signature    BLOB
);
CREATE INDEX IF NOT EXISTS idx_mail_pending
    ON mail (direction, delivered_at) WHERE delivered_at IS NULL;
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn peer() -> PeerId {
        Identity::generate().unwrap().peer_id()
    }

    #[test]
    fn first_sighting_is_new_then_known() {
        let store = Store::open_in_memory().unwrap();
        let alice = peer();

        assert_eq!(store.observe_peer(alice, "alice", 100).unwrap(), TrustState::New);
        assert_eq!(store.observe_peer(alice, "alice", 110).unwrap(), TrustState::Known);
    }

    #[test]
    fn a_new_key_wearing_a_known_name_is_flagged() {
        let store = Store::open_in_memory().unwrap();
        store.observe_peer(peer(), "alice", 100).unwrap();

        // Someone else turns up calling themselves alice.
        let mallory = peer();
        assert_eq!(
            store.observe_peer(mallory, "alice", 120).unwrap(),
            TrustState::NicknameConflict,
        );
    }

    #[test]
    fn renaming_yourself_is_not_an_impersonation() {
        let store = Store::open_in_memory().unwrap();
        let alice = peer();
        store.observe_peer(alice, "alice", 100).unwrap();

        // Same key, new label. This is a rename, and must stay trusted.
        assert_eq!(store.observe_peer(alice, "alice-laptop", 120).unwrap(), TrustState::Known);
    }

    #[test]
    fn verification_survives_later_sightings() {
        let store = Store::open_in_memory().unwrap();
        let alice = peer();
        store.observe_peer(alice, "alice", 100).unwrap();

        assert!(store.mark_verified(alice).unwrap());
        assert_eq!(store.observe_peer(alice, "alice", 130).unwrap(), TrustState::Verified);
    }

    #[test]
    fn hub_visits_upsert_and_order_by_recency() {
        let store = Store::open_in_memory().unwrap();
        let (first, second) = (peer(), peer());

        store.record_hub_visit(first, "basecamp", 100).unwrap();
        store.record_hub_visit(second, "library", 200).unwrap();
        store.record_hub_visit(first, "basecamp-renamed", 300).unwrap();

        let history = store.hub_history(10).unwrap();
        assert_eq!(history.len(), 2, "re-joining a hub updates rather than duplicates");
        assert_eq!(history[0].hub_name, "basecamp-renamed");
        assert_eq!(history[0].first_joined, 100, "original join time is preserved");
    }

    #[test]
    fn migration_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("intraweb.db");

        let store = Store::open(&path).unwrap();
        store.observe_peer(peer(), "alice", 100).unwrap();
        drop(store);

        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.known_peer_count().unwrap(), 1, "data survives reopen");
    }
}
