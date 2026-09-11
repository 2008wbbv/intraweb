//! UDP broadcast presence, for networks where multicast does not survive.
//!
//! mDNS is the better-mannered protocol, but a depressing number of consumer
//! access points drop multicast between wireless clients while happily passing
//! broadcast. Running both costs one small datagram every few seconds and turns
//! "it just doesn't find anything" into "it works".

use anyhow::{Context, Result};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use tokio::net::UdpSocket;

use crate::presence::MAX_BEACON_BYTES;

/// How often a node re-announces itself. Three seconds is the discovery budget
/// in the spec, so announce comfortably inside it.
pub const ANNOUNCE_INTERVAL_SECS: u64 = 2;

pub struct Beacon {
    socket: UdpSocket,
    port: u16,
}

impl Beacon {
    /// Bind the beacon port for both sending and receiving.
    ///
    /// Address and port reuse are enabled so several nodes can share a machine.
    /// That is not just a test convenience: an operator running a hub and a
    /// personal node on one Pi is an ordinary thing to want.
    pub fn bind(port: u16) -> Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .context("could not create the beacon socket")?;
        socket.set_reuse_address(true).context("could not set SO_REUSEADDR")?;
        #[cfg(all(unix, not(target_os = "solaris"), not(target_os = "illumos")))]
        socket.set_reuse_port(true).context("could not set SO_REUSEPORT")?;
        socket.set_broadcast(true).context("could not enable broadcast")?;
        socket.set_nonblocking(true)?;

        let bind_addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
        socket
            .bind(&bind_addr.into())
            .with_context(|| format!("could not bind the beacon to port {port}"))?;

        let socket = UdpSocket::from_std(socket.into())
            .context("could not hand the beacon socket to the async runtime")?;
        Ok(Self { socket, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Broadcast a presence datagram on every interface that has one.
    ///
    /// Returns how many destinations accepted it. A failure on one interface
    /// (a downed VPN tunnel, say) must not stop the others.
    pub async fn announce(&self, packet: &[u8]) -> usize {
        let mut delivered = 0;
        for target in broadcast_targets() {
            let dest = SocketAddr::V4(SocketAddrV4::new(target, self.port));
            match self.socket.send_to(packet, dest).await {
                Ok(_) => delivered += 1,
                Err(err) => tracing::debug!(%dest, %err, "beacon send failed on one interface"),
            }
        }
        delivered
    }

    /// Wait for the next datagram. Oversized reads are truncated by the buffer,
    /// and the parser rejects anything that does not then add up.
    pub async fn recv(&self) -> Result<(Vec<u8>, SocketAddr)> {
        let mut buf = vec![0u8; MAX_BEACON_BYTES];
        let (len, from) = self.socket.recv_from(&mut buf).await.context("beacon receive failed")?;
        buf.truncate(len);
        Ok((buf, from))
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }
}

/// Per-interface broadcast addresses, plus the global fallback.
///
/// The limited broadcast address alone is not enough: on a multi-homed host the
/// kernel picks one interface for it, so a node on the other subnet never hears
/// us. Sending to each interface's own broadcast address fixes that.
pub fn broadcast_targets() -> Vec<Ipv4Addr> {
    let mut targets = Vec::new();
    if let Ok(interfaces) = if_addrs::get_if_addrs() {
        for interface in interfaces {
            if interface.is_loopback() {
                continue;
            }
            if let if_addrs::IfAddr::V4(v4) = interface.addr
                && let Some(broadcast) = v4.broadcast
                && !targets.contains(&broadcast)
            {
                targets.push(broadcast);
            }
        }
    }
    targets.push(Ipv4Addr::BROADCAST);
    targets
}

/// Non-loopback IPv4 addresses of this host, for announcing where to reach us.
pub fn local_addresses() -> Vec<IpAddr> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    interfaces
        .into_iter()
        .filter(|interface| !interface.is_loopback())
        .map(|interface| interface.ip())
        .filter(|ip| ip.is_ipv4())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::Presence;
    use intraweb_core::Identity;
    use intraweb_core::peer::now_secs;

    /// Port 0 lets the OS pick, so tests never collide with a real daemon.
    async fn loopback_pair() -> (Beacon, u16) {
        let beacon = Beacon::bind(0).expect("bind should succeed");
        let port = beacon.local_addr().unwrap().port();
        (beacon, port)
    }

    #[tokio::test]
    async fn a_signed_presence_survives_the_wire() {
        let (beacon, port) = loopback_pair().await;
        let id = Identity::generate().unwrap();
        let presence = Presence::new(&id, "ben", 8420, 8421, true, "basecamp");
        let packet = presence.to_beacon(&presence.sign(&id)).unwrap();

        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sender.send_to(&packet, ("127.0.0.1", port)).await.unwrap();

        let (received, _from) = beacon.recv().await.unwrap();
        let (parsed, sig) = Presence::from_beacon(&received).unwrap();

        parsed.verify(&sig, now_secs()).unwrap();
        assert_eq!(parsed.nickname, "ben");
        assert!(parsed.is_hub);
    }

    #[tokio::test]
    async fn two_nodes_can_share_one_machine() {
        // An operator running a hub and a personal node on the same Pi.
        let first = Beacon::bind(0).unwrap();
        let port = first.local_addr().unwrap().port();
        let second = Beacon::bind(port);

        assert!(second.is_ok(), "port reuse must allow a second node on the same host");
    }

    #[test]
    fn broadcast_always_has_a_fallback_target() {
        let targets = broadcast_targets();
        assert!(targets.contains(&Ipv4Addr::BROADCAST), "must always try limited broadcast");
    }
}
