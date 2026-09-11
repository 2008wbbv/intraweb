//! Local storage. Every node's database is its own; nothing replicates.
//!
//! This is also the trust-on-first-use keyring. The job it does that matters
//! is noticing when a familiar nickname turns up carrying an unfamiliar key.

use crate::identity::PeerId;
use crate::mail::{Mail, SignedMail};
use crate::peer::TrustState;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

const SCHEMA_VERSION: i64 = 2;

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

    /// Apply each migration the database has not seen yet.
    ///
    /// Steps are additive and run in order, so a vault created by any earlier
    /// release opens without the operator doing anything.
    fn migrate(&self) -> Result<()> {
        let current: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (version, sql) in [(1, SCHEMA_V1), (2, SCHEMA_V2)] {
            if current >= version {
                continue;
            }
            self.conn
                .execute_batch(sql)
                .with_context(|| format!("could not apply database migration {version}"))?;
        }
        if current < SCHEMA_VERSION {
            self.conn
                .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(())
    }

    /// Record a sighting and report what we can honestly say about the peer.
    ///
    /// Called on every discovery packet, so it must stay cheap and idempotent.
    pub fn observe_peer(&self, peer_id: PeerId, nickname: &str, now: u64) -> Result<TrustState> {
        let hex = peer_id.to_hex();

        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT verified FROM known_peers WHERE peer_id = ?1",
                params![hex],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(verified) = existing {
            self.conn.execute(
                "UPDATE known_peers SET nickname = ?2, last_seen = ?3 WHERE peer_id = ?1",
                params![hex, nickname, now as i64],
            )?;
            return Ok(if verified != 0 {
                TrustState::Verified
            } else {
                TrustState::Known
            });
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

        Ok(if impostor.is_some() {
            TrustState::NicknameConflict
        } else {
            TrustState::New
        })
    }

    /// Mark that a human compared fingerprints out of band and they matched.
    pub fn mark_verified(&self, peer_id: PeerId) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE known_peers SET verified = 1 WHERE peer_id = ?1",
            params![peer_id.to_hex()],
        )?;
        Ok(changed > 0)
    }

    /// Everyone this vault has ever seen, most recently first.
    ///
    /// Mail can be addressed to someone who is not in range -- that is the
    /// entire point of a queue -- so addressing consults this as well as the
    /// live roster.
    pub fn known_peers(&self, limit: u32) -> Result<Vec<KnownPeer>> {
        let mut stmt = self.conn.prepare(
            "SELECT peer_id, nickname, verified FROM known_peers
             ORDER BY last_seen DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            let hex: String = row.get(0)?;
            Ok(KnownPeer {
                peer_id: PeerId::parse_hex(&hex).unwrap_or_else(|| PeerId::from_bytes([0u8; 32])),
                nickname: row.get(1)?,
                verified: row.get::<_, i64>(2)? != 0,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn known_peer_count(&self) -> Result<u64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM known_peers", [], |r| r.get(0))?;
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

    // ---- mail -------------------------------------------------------------

    /// Put a message in the outbox. Delivery happens when the recipient shows up.
    pub fn queue_outgoing(&self, mail: &Mail) -> Result<i64> {
        mail.check_limits()?;
        self.conn.execute(
            "INSERT INTO mail (direction, peer_id, subject, body, created_at)
             VALUES ('out', ?1, ?2, ?3, ?4)",
            params![
                mail.to.to_hex(),
                mail.subject,
                mail.body,
                mail.created_at as i64
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Messages still waiting for their recipient to appear.
    pub fn pending_outgoing(&self) -> Result<Vec<StoredMail>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, peer_id, subject, body, created_at, delivered_at, read_at
             FROM mail WHERE direction = 'out' AND delivered_at IS NULL
             ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], stored_mail)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn mark_delivered(&self, id: i64, now: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE mail SET delivered_at = ?2 WHERE id = ?1",
            params![id, now as i64],
        )?;
        Ok(())
    }

    /// File a verified incoming message.
    ///
    /// Returns false when we already hold it. The sender retries until it gets
    /// an acknowledgement, so a reply lost on the way back would otherwise
    /// deliver the same message twice.
    pub fn store_incoming(&self, sealed: &SignedMail, now: u64) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO mail
                (direction, peer_id, subject, body, created_at, delivered_at, signature)
             VALUES ('in', ?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                sealed.mail.from.to_hex(),
                sealed.mail.subject,
                sealed.mail.body,
                sealed.mail.created_at as i64,
                now as i64,
                sealed.signature,
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn inbox(&self, limit: u32) -> Result<Vec<StoredMail>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, peer_id, subject, body, created_at, delivered_at, read_at
             FROM mail WHERE direction = 'in' ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], stored_mail)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn outbox(&self, limit: u32) -> Result<Vec<StoredMail>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, peer_id, subject, body, created_at, delivered_at, read_at
             FROM mail WHERE direction = 'out' ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], stored_mail)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn unread_count(&self) -> Result<u64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM mail WHERE direction = 'in' AND read_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(count as u64)
    }

    pub fn mark_read(&self, id: i64, now: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE mail SET read_at = ?2 WHERE id = ?1 AND read_at IS NULL",
            params![id, now as i64],
        )?;
        Ok(())
    }

    /// Mark every received message read. Returns how many changed.
    pub fn mark_all_read(&self, now: u64) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE mail SET read_at = ?1 WHERE direction = 'in' AND read_at IS NULL",
            params![now as i64],
        )?)
    }
}

fn stored_mail(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMail> {
    let peer_hex: String = row.get(1)?;
    Ok(StoredMail {
        id: row.get(0)?,
        peer_id: PeerId::parse_hex(&peer_hex).unwrap_or_else(|| PeerId::from_bytes([0u8; 32])),
        subject: row.get(2)?,
        body: row.get(3)?,
        created_at: row.get::<_, i64>(4)? as u64,
        delivered_at: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
        read_at: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
    })
}

/// Somebody this vault has met before, whether or not they are in range now.
#[derive(Debug, Clone, serde::Serialize)]
pub struct KnownPeer {
    pub peer_id: PeerId,
    pub nickname: String,
    pub verified: bool,
}

/// A message as stored locally.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoredMail {
    pub id: i64,
    /// For received mail this is the sender; for sent mail, the recipient.
    pub peer_id: PeerId,
    pub subject: String,
    pub body: String,
    pub created_at: u64,
    /// When it actually reached the recipient's node. `None` means still queued.
    pub delivered_at: Option<u64>,
    pub read_at: Option<u64>,
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

/// v2: unread tracking, and a guard against filing the same letter twice.
const SCHEMA_V2: &str = r#"
ALTER TABLE mail ADD COLUMN read_at INTEGER;

-- The sender retries until acknowledged, so an acknowledgement lost on the way
-- back would otherwise deliver the same message again.
CREATE UNIQUE INDEX IF NOT EXISTS idx_mail_inbound_unique
    ON mail (signature) WHERE direction = 'in' AND signature IS NOT NULL;
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

        assert_eq!(
            store.observe_peer(alice, "alice", 100).unwrap(),
            TrustState::New
        );
        assert_eq!(
            store.observe_peer(alice, "alice", 110).unwrap(),
            TrustState::Known
        );
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
        assert_eq!(
            store.observe_peer(alice, "alice-laptop", 120).unwrap(),
            TrustState::Known
        );
    }

    #[test]
    fn verification_survives_later_sightings() {
        let store = Store::open_in_memory().unwrap();
        let alice = peer();
        store.observe_peer(alice, "alice", 100).unwrap();

        assert!(store.mark_verified(alice).unwrap());
        assert_eq!(
            store.observe_peer(alice, "alice", 130).unwrap(),
            TrustState::Verified
        );
    }

    #[test]
    fn hub_visits_upsert_and_order_by_recency() {
        let store = Store::open_in_memory().unwrap();
        let (first, second) = (peer(), peer());

        store.record_hub_visit(first, "basecamp", 100).unwrap();
        store.record_hub_visit(second, "library", 200).unwrap();
        store
            .record_hub_visit(first, "basecamp-renamed", 300)
            .unwrap();

        let history = store.hub_history(10).unwrap();
        assert_eq!(
            history.len(),
            2,
            "re-joining a hub updates rather than duplicates"
        );
        assert_eq!(history[0].hub_name, "basecamp-renamed");
        assert_eq!(
            history[0].first_joined, 100,
            "original join time is preserved"
        );
    }

    fn letter(from: &Identity, to: &Identity, subject: &str, body: &str, at: u64) -> SignedMail {
        let mail = Mail::new(from.peer_id(), to.peer_id(), subject, body, at);
        SignedMail::seal(mail, from).unwrap()
    }

    #[test]
    fn a_queued_letter_waits_then_is_marked_delivered() {
        let store = Store::open_in_memory().unwrap();
        let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let mail = Mail::new(alice.peer_id(), bob.peer_id(), "fuel", "thursday", 100);

        let id = store.queue_outgoing(&mail).unwrap();
        assert_eq!(store.pending_outgoing().unwrap().len(), 1);

        store.mark_delivered(id, 150).unwrap();
        assert!(
            store.pending_outgoing().unwrap().is_empty(),
            "delivered mail stops retrying"
        );
        assert_eq!(store.outbox(10).unwrap()[0].delivered_at, Some(150));
    }

    #[test]
    fn the_same_letter_is_never_filed_twice() {
        let store = Store::open_in_memory().unwrap();
        let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let sealed = letter(&alice, &bob, "fuel", "thursday", 100);

        assert!(
            store.store_incoming(&sealed, 110).unwrap(),
            "first copy is filed"
        );
        // The sender retries when our acknowledgement goes missing.
        assert!(
            !store.store_incoming(&sealed, 120).unwrap(),
            "the retry is recognised"
        );
        assert_eq!(store.inbox(10).unwrap().len(), 1);
    }

    #[test]
    fn two_different_letters_both_arrive() {
        let store = Store::open_in_memory().unwrap();
        let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());

        store
            .store_incoming(&letter(&alice, &bob, "one", "body", 100), 100)
            .unwrap();
        store
            .store_incoming(&letter(&alice, &bob, "two", "body", 101), 101)
            .unwrap();

        assert_eq!(store.inbox(10).unwrap().len(), 2);
    }

    #[test]
    fn unread_counts_track_reading() {
        let store = Store::open_in_memory().unwrap();
        let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        store
            .store_incoming(&letter(&alice, &bob, "one", "b", 100), 100)
            .unwrap();
        store
            .store_incoming(&letter(&alice, &bob, "two", "b", 101), 101)
            .unwrap();
        assert_eq!(store.unread_count().unwrap(), 2);

        let first = store.inbox(10).unwrap()[0].id;
        store.mark_read(first, 200).unwrap();
        assert_eq!(store.unread_count().unwrap(), 1);

        assert_eq!(
            store.mark_all_read(210).unwrap(),
            1,
            "only the still-unread one changes"
        );
        assert_eq!(store.unread_count().unwrap(), 0);
    }

    #[test]
    fn the_inbox_shows_the_sender_and_the_outbox_the_recipient() {
        let store = Store::open_in_memory().unwrap();
        let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());

        store
            .store_incoming(&letter(&alice, &bob, "hi", "b", 100), 100)
            .unwrap();
        store
            .queue_outgoing(&Mail::new(bob.peer_id(), alice.peer_id(), "re", "b", 101))
            .unwrap();

        assert_eq!(store.inbox(10).unwrap()[0].peer_id, alice.peer_id());
        assert_eq!(store.outbox(10).unwrap()[0].peer_id, alice.peer_id());
    }

    #[test]
    fn a_vault_created_before_mail_existed_still_opens() {
        // Exactly what an operator who installed the first release has on disk.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("intraweb.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
        }

        let store = Store::open(&path).unwrap();

        // The v2 column and index must now exist and work.
        let (alice, bob) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        assert!(
            store
                .store_incoming(&letter(&alice, &bob, "s", "b", 100), 100)
                .unwrap()
        );
        assert_eq!(store.unread_count().unwrap(), 1);
    }

    #[test]
    fn migration_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("intraweb.db");

        let store = Store::open(&path).unwrap();
        store.observe_peer(peer(), "alice", 100).unwrap();
        drop(store);

        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.known_peer_count().unwrap(),
            1,
            "data survives reopen"
        );
    }
}
