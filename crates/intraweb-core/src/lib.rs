//! Core types for intraweb: your neighborhood web.
//!
//! The organizing idea is that a person is a keypair and a folder, and a hub is
//! only a place to meet. Nothing here knows how to talk to a network; that is
//! `intraweb-net`'s job. Keeping the split lets the discovery layer be swapped
//! or tested without touching identity or storage.

pub mod config;
pub mod db;
pub mod hex;
pub mod identity;
pub mod peer;
pub mod vault;

pub use config::Config;
pub use db::Store;
pub use identity::{Identity, PeerId};
pub use peer::{DiscoverySource, Peer, TrustState, now_secs};
pub use vault::Vault;

/// Wire-protocol version. Bumped when the discovery payload changes shape, so
/// a node from a future release is skipped rather than misread.
pub const PROTOCOL_VERSION: u16 = 1;

/// mDNS service type every node registers and browses for.
pub const SERVICE_TYPE: &str = "_intraweb._tcp.local.";
