mod crates;
pub mod data_channel;

// re-export sub-crates
pub use crates::dtls;
pub use crates::ice;
pub use crates::sctp;
pub use crates::sdp;
pub use crates::stun;
pub use crates::util;

pub(crate) mod api;
pub(crate) mod dtls_transport;
pub(crate) mod error;
pub(crate) mod ice_transport;
pub(crate) mod mux;
pub(crate) mod peer_connection;
pub(crate) mod sctp_transport;

pub(crate) const UNSPECIFIED_STR: &str = "Unspecified";

/// Equal to UDP MTU
pub(crate) const RECEIVE_MTU: usize = 1460;
