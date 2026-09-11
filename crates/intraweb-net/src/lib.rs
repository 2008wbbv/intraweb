//! Zero-config discovery for intraweb.
//!
//! Two announcement paths run side by side. mDNS is the well-behaved one that
//! also gives us `.local` names. The UDP broadcast beacon exists because plenty
//! of consumer access points quietly drop multicast, and a discovery system
//! that fails silently on ordinary hardware is not a discovery system.

pub mod beacon;
pub mod doctor;
pub mod mdns;
pub mod node;
pub mod presence;
pub mod registry;

pub use presence::Presence;
pub use node::{Node, Roster};
pub use registry::{PeerRegistry, Sighting};
