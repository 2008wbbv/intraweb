//! Network diagnosis.
//!
//! Discovery failures on real hardware are almost never bugs in the discovery
//! code. They are access points isolating wireless clients, guest networks
//! filtering multicast, or a system Avahi already holding port 5353. Every one
//! of those looks identical from the dashboard: an empty roster.
//!
//! This module's only job is to tell the difference and say so in plain words.

use crate::beacon::{Beacon, local_addresses};
use crate::presence::Presence;
use anyhow::Result;
use intraweb_core::{Identity, SERVICE_TYPE};
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    /// What we actually observed. Never a guess.
    pub detail: String,
    /// What a person can do about it, when there is something to do.
    pub remedy: Option<String>,
}

impl Check {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Pass,
            detail: detail.into(),
            remedy: None,
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Warn,
            detail: detail.into(),
            remedy: Some(remedy.into()),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Fail,
            detail: detail.into(),
            remedy: Some(remedy.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
    /// Plain-English reading of the checks taken together.
    pub verdict: String,
    pub mdns_peers: usize,
    pub beacon_peers: usize,
}

impl Report {
    pub fn worst_status(&self) -> Status {
        if self.checks.iter().any(|c| c.status == Status::Fail) {
            Status::Fail
        } else if self.checks.iter().any(|c| c.status == Status::Warn) {
            Status::Warn
        } else {
            Status::Pass
        }
    }
}

/// Run every check, listening for `listen` for signs of other nodes.
pub async fn run(beacon_port: u16, listen: Duration) -> Result<Report> {
    let mut checks = Vec::new();

    // 1. Are we on a network at all?
    let addrs = local_addresses();
    if addrs.is_empty() {
        checks.push(Check::fail(
            "network interface",
            "no non-loopback IPv4 address found",
            "connect this device to the Wi-Fi or plug in the ethernet cable",
        ));
    } else {
        let list: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
        checks.push(Check::pass(
            "network interface",
            format!("reachable at {}", list.join(", ")),
        ));
    }

    // 2. Can we hold the beacon port?
    let beacon = match Beacon::bind(beacon_port) {
        Ok(beacon) => {
            checks.push(Check::pass(
                "beacon port",
                format!("bound udp/{beacon_port}"),
            ));
            Some(beacon)
        }
        Err(err) => {
            checks.push(Check::fail(
                "beacon port",
                format!("could not bind udp/{beacon_port}: {err}"),
                format!("another program is using udp/{beacon_port}; stop it or set beacon_port in config.toml"),
            ));
            None
        }
    };

    // 3. Does the mDNS responder come up? This is where a system Avahi already
    //    holding 5353 announces itself as a problem.
    match mdns_sd::ServiceDaemon::new() {
        Ok(daemon) => {
            checks.push(Check::pass(
                "mdns responder",
                "started and holding udp/5353",
            ));
            let _ = daemon.shutdown();
        }
        Err(err) => checks.push(Check::warn(
            "mdns responder",
            format!("could not start: {err}"),
            "a system mDNS daemon (avahi-daemon or mDNSResponder) may already hold udp/5353; \
             intraweb still works over the UDP beacon, but .local names will not resolve",
        )),
    }

    // 4. Listen on both paths at once and see what actually arrives.
    let (mdns_peers, beacon_peers) = observe(beacon.as_ref(), listen).await;

    checks.push(match mdns_peers {
        0 => Check::warn(
            "mdns peers",
            "heard nothing over multicast",
            "either no other node is running, or this network drops multicast between clients",
        ),
        n => Check::pass("mdns peers", format!("heard {n} over multicast")),
    });

    checks.push(match beacon_peers {
        0 => Check::warn(
            "beacon peers",
            "heard nothing over broadcast",
            "either no other node is running, or this network blocks client-to-client traffic",
        ),
        n => Check::pass("beacon peers", format!("heard {n} over broadcast")),
    });

    let verdict = verdict_for(addrs.is_empty(), mdns_peers, beacon_peers);
    Ok(Report {
        checks,
        verdict,
        mdns_peers,
        beacon_peers,
    })
}

/// Listen on both discovery paths concurrently and count distinct peers.
async fn observe(beacon: Option<&Beacon>, listen: Duration) -> (usize, usize) {
    use std::collections::HashSet;

    let mdns_task = async move {
        let mut seen = HashSet::new();
        let Ok(daemon) = mdns_sd::ServiceDaemon::new() else {
            return seen;
        };
        let Ok(events) = daemon.browse(SERVICE_TYPE) else {
            return seen;
        };
        let deadline = std::time::Instant::now() + listen;
        while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
            match events.recv_timeout(remaining) {
                Ok(mdns_sd::ServiceEvent::ServiceResolved(service)) => {
                    let txt = service.txt_properties.clone().into_property_map_str();
                    if let Some(pk) = txt.get("pk") {
                        seen.insert(pk.clone());
                    }
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        let _ = daemon.shutdown();
        seen
    };

    let beacon_task = async {
        let mut seen = HashSet::new();
        let Some(beacon) = beacon else {
            return seen;
        };
        let _ = tokio::time::timeout(listen, async {
            loop {
                let Ok((packet, _from)) = beacon.recv().await else {
                    continue;
                };
                if let Ok((presence, _sig)) = Presence::from_beacon(&packet) {
                    seen.insert(presence.peer_id.to_hex());
                }
            }
        })
        .await;
        seen
    };

    // mDNS browsing is blocking, so it gets its own thread rather than stalling
    // the beacon listener for the whole window.
    let mdns_handle = tokio::task::spawn_blocking(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|rt| rt.block_on(mdns_task))
            .unwrap_or_default()
    });

    let beacon_seen = beacon_task.await;
    let mdns_seen = mdns_handle.await.unwrap_or_default();
    (mdns_seen.len(), beacon_seen.len())
}

/// Interpret the counts. Stated as possibilities, because one node cannot prove
/// what an access point is doing -- it can only narrow it down honestly.
fn verdict_for(no_interface: bool, mdns_peers: usize, beacon_peers: usize) -> String {
    if no_interface {
        return "This device is not on a network. Nothing can be discovered until it is."
            .to_string();
    }
    match (mdns_peers, beacon_peers) {
        (0, 0) => "No other nodes heard. If you are the first one up, this is expected. \
                   If another node is definitely running on this Wi-Fi, the access point is \
                   almost certainly isolating clients -- look for 'AP isolation', \
                   'client isolation', or guest-network mode in the router settings."
            .to_string(),
        (0, n) => format!(
            "Heard {n} node(s) over broadcast but none over multicast. This network filters \
             multicast, so mDNS and .local names will not work here -- intraweb will keep \
             running on the UDP beacon. Reach peers by IP address rather than by name."
        ),
        (n, 0) => format!(
            "Heard {n} node(s) over multicast but none over broadcast. mDNS and .local names \
             work; the broadcast fallback is being filtered. Nothing to fix."
        ),
        (m, b) => format!(
            "Both discovery paths are working: {m} node(s) over multicast, {b} over broadcast."
        ),
    }
}

/// Confirm we can actually put a signed packet on the wire. Used by `doctor`
/// to separate "nothing to hear" from "we cannot even transmit".
pub async fn self_test_broadcast(beacon: &Beacon, identity: &Identity) -> Result<usize> {
    let presence = Presence::new(identity, "doctor", 0, false, "");
    let packet = presence.to_beacon(&presence.sign(identity))?;
    Ok(beacon.announce(&packet).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beacon::broadcast_targets;

    #[test]
    fn silence_everywhere_points_at_client_isolation() {
        let verdict = verdict_for(false, 0, 0);
        assert!(
            verdict.contains("isolating clients"),
            "operators need the actual router term"
        );
    }

    #[test]
    fn broadcast_only_diagnoses_multicast_filtering() {
        let verdict = verdict_for(false, 0, 2);
        assert!(verdict.contains("filters multicast"));
        assert!(
            verdict.contains(".local"),
            "must warn that names stop working"
        );
    }

    #[test]
    fn multicast_only_is_not_alarming() {
        let verdict = verdict_for(false, 3, 0);
        assert!(verdict.contains("Nothing to fix"));
    }

    #[test]
    fn a_healthy_network_says_so() {
        assert!(verdict_for(false, 2, 2).contains("Both discovery paths are working"));
    }

    #[test]
    fn no_interface_short_circuits_everything() {
        let verdict = verdict_for(true, 0, 0);
        assert!(verdict.contains("not on a network"));
    }

    #[test]
    fn broadcast_targets_are_never_empty() {
        assert!(
            !broadcast_targets().is_empty(),
            "there is always a fallback target"
        );
    }

    /// Deliberately makes no claim about how many nodes are around: the machine
    /// running the tests may well have real nodes on it, and a diagnostic that
    /// only passes on an empty network is not testing anything useful.
    #[tokio::test]
    async fn doctor_reports_honestly_whoever_else_is_on_the_network() {
        let report = run(0, Duration::from_millis(250)).await.unwrap();

        assert!(
            !report.checks.is_empty(),
            "a report with no checks tells nobody anything"
        );

        // Whatever it heard, the verdict must be the reading of those counts.
        assert_eq!(
            report.verdict,
            verdict_for(
                local_addresses().is_empty(),
                report.mdns_peers,
                report.beacon_peers
            ),
            "the verdict must follow from the counts it reported",
        );

        // The whole point of this command: never say something is wrong
        // without saying what to do about it.
        for check in &report.checks {
            if check.status != Status::Pass {
                assert!(
                    check.remedy.is_some(),
                    "check '{}' warns with no remedy",
                    check.name
                );
            }
        }
    }

    #[tokio::test]
    async fn a_quiet_network_is_never_a_hard_failure() {
        // Being first to arrive is normal, not an error worth exiting nonzero.
        let checks = vec![
            Check::pass("network interface", "reachable at 192.0.2.2"),
            Check::warn(
                "mdns peers",
                "heard nothing over multicast",
                "nobody else may be up",
            ),
        ];
        let report = Report {
            checks,
            verdict: verdict_for(false, 0, 0),
            mdns_peers: 0,
            beacon_peers: 0,
        };

        assert_eq!(report.worst_status(), Status::Warn);
    }
}
