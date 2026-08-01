//! Neighbor discovery, presence, and remembered routes.

pub use crate::lifecycle::{Neighbor as State, Reachability};
pub use crate::neighbor_presence::{
    Invalid, PresenceAck, PresenceProbe, MAX_MESSAGE, PRESENCE_ALPN, PRESENCE_PROTOCOL,
};
pub use crate::neighbors::{NeighborRecord as Record, NeighborRegistry as Catalog, StoredRoute};
