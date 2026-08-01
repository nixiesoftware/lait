//! Exact Station identity and activation coordinates.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::{DeviceId, SpaceId};

/// The stable validated device-key role occupied by a Station.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Key([u8; 32]);

impl Key {
    pub fn from_key_bytes(raw: [u8; 32]) -> Self {
        Self(raw)
    }

    pub fn from_device(device: &DeviceId) -> Option<Self> {
        device.key_bytes().map(Self)
    }

    pub fn key_bytes(&self) -> [u8; 32] {
        self.0
    }

    pub fn as_device(&self) -> DeviceId {
        DeviceId::from_key_bytes(&self.0)
    }

    pub fn short(&self) -> String {
        data_encoding::HEXLOWER.encode(&self.0[..4])
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&data_encoding::HEXLOWER.encode(&self.0))
    }
}

/// The durable coordinate of a Station within one Space.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Address {
    pub space: SpaceId,
    pub key: Key,
}

/// A fresh activation discriminator, durably increased before every opening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Epoch(u64);

impl Epoch {
    pub const ZERO: Self = Self(0);

    pub fn from_u64(value: u64) -> Self {
        Self(value)
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl fmt::Display for Epoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One activation of a Station address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Instance {
    pub address: Address,
    pub epoch: Epoch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_compare_by_device_key() {
        let raw = [7u8; 32];
        let key = Key::from_key_bytes(raw);
        let device = key.as_device();
        assert_eq!(Key::from_device(&device), Some(key.clone()));
        assert_eq!(key.key_bytes(), raw);
    }

    #[test]
    fn addresses_and_instances_include_their_discriminators() {
        let a = Address {
            space: SpaceId::from_digest([1; 16]),
            key: Key::from_key_bytes([7; 32]),
        };
        let b = Address {
            space: SpaceId::from_digest([2; 16]),
            key: a.key.clone(),
        };
        assert_ne!(a, b);
        assert_ne!(
            Instance {
                address: a.clone(),
                epoch: Epoch::from_u64(1)
            },
            Instance {
                address: a,
                epoch: Epoch::from_u64(2)
            }
        );
        assert_eq!(Epoch::from_u64(u64::MAX).next(), None);
    }
}
