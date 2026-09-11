//! The live roster of who is on the network right now.
//!
//! This is deliberately in-memory and deliberately forgetful. A peer that stops
//! announcing falls off; nothing here is replicated to anyone. Durable facts
//! (which keys we have met before) live in the keyring, not in this table.

use crate::presence::Presence;
use intraweb_core::identity::PeerId;
use intraweb_core::peer::{DiscoverySource, Peer, TrustState};
use std::collections::HashMap;
use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sighting {
    /// A peer we did not have on the roster a moment ago.
    Arrived,
    /// A peer we already knew about, refreshed.
    Refreshed,
    /// Our own announcement, echoed back to us. Ignored.
    SelfEcho,
}

pub struct PeerRegistry {
    self_id: PeerId,
    timeout_secs: u64,
    peers: HashMap<PeerId, Peer>,
}

impl PeerRegistry {
    pub fn new(self_id: PeerId, timeout_secs: u64) -> Self {
        Self { self_id, timeout_secs, peers: HashMap::new() }
    }

    /// Fold a verified presence record into the roster.
    ///
    /// The caller must have checked the signature already; the registry is
    /// about liveness, not authenticity.
    pub fn record(
        &mut self,
        presence: &Presence,
        addrs: Vec<IpAddr>,
        source: DiscoverySource,
        trust: TrustState,
        now: u64,
    ) -> Sighting {
        if presence.peer_id == self.self_id {
            return Sighting::SelfEcho;
        }

        match self.peers.get_mut(&presence.peer_id) {
            Some(existing) => {
                existing.nickname = presence.nickname.clone();
                existing.api_port = presence.api_port;
                existing.transport_port = presence.transport_port;
                existing.is_hub = presence.is_hub;
                existing.hub_name = hub_name(presence);
                existing.source = existing.source.merge(source);
                existing.last_seen = now;
                // Never downgrade a trust judgement we already made: a peer
                // flagged as a nickname conflict stays flagged for the session.
                if existing.trust != TrustState::NicknameConflict {
                    existing.trust = trust;
                }
                for addr in addrs {
                    if !existing.addrs.contains(&addr) {
                        existing.addrs.push(addr);
                    }
                }
                Sighting::Refreshed
            }
            None => {
                self.peers.insert(
                    presence.peer_id,
                    Peer {
                        peer_id: presence.peer_id,
                        fingerprint: presence.peer_id.fingerprint(),
                        nickname: presence.nickname.clone(),
                        addrs,
                        api_port: presence.api_port,
                        transport_port: presence.transport_port,
                        is_hub: presence.is_hub,
                        hub_name: hub_name(presence),
                        source,
                        trust,
                        first_seen: now,
                        last_seen: now,
                    },
                );
                Sighting::Arrived
            }
        }
    }

    /// Drop peers we have not heard from inside the timeout. Returns who left,
    /// so the caller can log departures rather than silently shrinking the list.
    pub fn sweep(&mut self, now: u64) -> Vec<Peer> {
        let timeout = self.timeout_secs;
        let departed: Vec<Peer> = self
            .peers
            .values()
            .filter(|peer| peer.is_stale(now, timeout))
            .cloned()
            .collect();
        for peer in &departed {
            self.peers.remove(&peer.peer_id);
        }
        departed
    }

    /// Current roster: hubs first, then alphabetically, so the list a person
    /// reads does not reshuffle itself every refresh.
    pub fn snapshot(&self) -> Vec<Peer> {
        let mut peers: Vec<Peer> = self.peers.values().cloned().collect();
        peers.sort_by(|a, b| {
            b.is_hub
                .cmp(&a.is_hub)
                .then_with(|| a.nickname.cmp(&b.nickname))
                .then_with(|| a.peer_id.cmp(&b.peer_id))
        });
        peers
    }

    /// Every hub currently reachable. Joining all of them at once is the point:
    /// one vault, many neighborhoods.
    pub fn hubs(&self) -> Vec<Peer> {
        self.snapshot().into_iter().filter(|peer| peer.is_hub).collect()
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

fn hub_name(presence: &Presence) -> Option<String> {
    presence.is_hub.then(|| presence.hub_name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use intraweb_core::Identity;
    use std::net::Ipv4Addr;

    fn addr(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, last))
    }

    fn presence(id: &Identity, nick: &str, is_hub: bool) -> Presence {
        Presence::new(id, nick, 8420, 8421, is_hub, "basecamp")
    }

    fn registry() -> (PeerRegistry, Identity) {
        let me = Identity::generate().unwrap();
        (PeerRegistry::new(me.peer_id(), 30), me)
    }

    #[test]
    fn arrival_then_refresh() {
        let (mut reg, _me) = registry();
        let alice = Identity::generate().unwrap();
        let p = presence(&alice, "alice", false);

        let first = reg.record(&p, vec![addr(10)], DiscoverySource::Mdns, TrustState::New, 100);
        let second = reg.record(&p, vec![addr(10)], DiscoverySource::Mdns, TrustState::Known, 110);

        assert_eq!(first, Sighting::Arrived);
        assert_eq!(second, Sighting::Refreshed);
        assert_eq!(reg.len(), 1, "the same key must not appear twice");
    }

    #[test]
    fn our_own_echo_is_ignored() {
        let (mut reg, me) = registry();
        let mine = presence(&me, "me", false);

        let outcome = reg.record(&mine, vec![addr(5)], DiscoverySource::Beacon, TrustState::New, 100);

        assert_eq!(outcome, Sighting::SelfEcho);
        assert!(reg.is_empty(), "we must not list ourselves as a neighbor");
    }

    #[test]
    fn seeing_a_peer_both_ways_merges_the_source() {
        let (mut reg, _me) = registry();
        let alice = Identity::generate().unwrap();
        let p = presence(&alice, "alice", false);

        reg.record(&p, vec![addr(10)], DiscoverySource::Mdns, TrustState::New, 100);
        reg.record(&p, vec![addr(10)], DiscoverySource::Beacon, TrustState::Known, 101);

        assert_eq!(reg.snapshot()[0].source, DiscoverySource::Both);
    }

    #[test]
    fn addresses_accumulate_without_duplicates() {
        let (mut reg, _me) = registry();
        let alice = Identity::generate().unwrap();
        let p = presence(&alice, "alice", false);

        reg.record(&p, vec![addr(10)], DiscoverySource::Mdns, TrustState::New, 100);
        reg.record(&p, vec![addr(10), addr(11)], DiscoverySource::Mdns, TrustState::Known, 101);

        assert_eq!(reg.snapshot()[0].addrs, vec![addr(10), addr(11)]);
    }

    #[test]
    fn a_conflict_flag_is_never_quietly_downgraded() {
        let (mut reg, _me) = registry();
        let mallory = Identity::generate().unwrap();
        let p = presence(&mallory, "alice", false);

        reg.record(&p, vec![addr(66)], DiscoverySource::Mdns, TrustState::NicknameConflict, 100);
        reg.record(&p, vec![addr(66)], DiscoverySource::Mdns, TrustState::Known, 110);

        assert_eq!(
            reg.snapshot()[0].trust,
            TrustState::NicknameConflict,
            "a warning must not evaporate just because the peer kept announcing",
        );
    }

    #[test]
    fn stale_peers_are_swept_and_reported() {
        let (mut reg, _me) = registry();
        let alice = Identity::generate().unwrap();
        let bob = Identity::generate().unwrap();

        reg.record(&presence(&alice, "alice", false), vec![], DiscoverySource::Mdns, TrustState::New, 100);
        reg.record(&presence(&bob, "bob", false), vec![], DiscoverySource::Mdns, TrustState::New, 180);

        let departed = reg.sweep(200);

        assert_eq!(departed.len(), 1);
        assert_eq!(departed[0].nickname, "alice");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn hubs_sort_first_so_the_roster_reads_sensibly() {
        let (mut reg, _me) = registry();
        let zoe = Identity::generate().unwrap();
        let hub = Identity::generate().unwrap();

        reg.record(&presence(&zoe, "zoe", false), vec![], DiscoverySource::Mdns, TrustState::New, 100);
        reg.record(&presence(&hub, "library", true), vec![], DiscoverySource::Mdns, TrustState::New, 100);

        let snapshot = reg.snapshot();
        assert!(snapshot[0].is_hub);
        assert_eq!(snapshot[0].nickname, "library");
        assert_eq!(reg.hubs().len(), 1);
    }

    #[test]
    fn multiple_hubs_are_all_listed() {
        let (mut reg, _me) = registry();
        for name in ["basecamp", "library", "firehouse"] {
            let hub = Identity::generate().unwrap();
            reg.record(&presence(&hub, name, true), vec![], DiscoverySource::Mdns, TrustState::New, 100);
        }
        // One vault, many neighborhoods: we never pick just one.
        assert_eq!(reg.hubs().len(), 3);
    }
}
