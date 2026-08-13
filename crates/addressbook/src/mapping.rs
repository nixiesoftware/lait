//! Fabric path and entry strings. Frozen in tests.

use crate::ids::CardId;
use mechanics::ids::DeviceId;

/// The one Body this book occupies.
pub const BODY_KEY: &[u8] = b"lait/addressbook/v1";

/// Map of schema and clocks.
pub const PATH_META: &str = "meta";
pub const ENTRY_SCHEMA: &str = "schema";
pub const ENTRY_CLOCK: &str = "clock";

/// Graves: entry is the Card id.
pub const PATH_GRAVES: &str = "graves";
/// Redirects: entry is the source Card id.
pub const PATH_REDIRECTS: &str = "redirects";

pub fn entry_tag_counter(device: &DeviceId) -> String {
    format!("tag:{}", device.as_str())
}

pub fn path_name(id: &CardId) -> String {
    format!("card/{}/name", id.as_str())
}

pub fn path_note(id: &CardId) -> String {
    format!("card/{}/note", id.as_str())
}

pub fn path_created(id: &CardId) -> String {
    format!("card/{}/created", id.as_str())
}

pub fn path_self(id: &CardId) -> String {
    format!("card/{}/self", id.as_str())
}

pub fn path_groups(id: &CardId) -> String {
    format!("card/{}/groups", id.as_str())
}

pub fn path_handles(id: &CardId) -> String {
    format!("card/{}/handles", id.as_str())
}

pub fn entry_device(device: &DeviceId) -> String {
    device.as_str().to_owned()
}

pub fn card_id_entry(id: &CardId) -> String {
    id.as_str().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::ids::{DeviceId, UlidSource};

    struct Seq;
    impl UlidSource for Seq {
        fn now_ms(&self) -> u64 {
            1
        }
        fn rand80(&self) -> u128 {
            1
        }
    }

    #[test]
    fn the_body_key_and_paths_are_the_frozen_v1_spellings() {
        assert_eq!(BODY_KEY, b"lait/addressbook/v1");
        assert_eq!(PATH_META, "meta");
        assert_eq!(PATH_GRAVES, "graves");
        assert_eq!(PATH_REDIRECTS, "redirects");
        let id = CardId::mint(&Seq);
        assert!(path_name(&id).starts_with("card/crd_"));
        assert!(path_name(&id).ends_with("/name"));
        let device = DeviceId::from_key_bytes(&[0xab; 32]);
        assert_eq!(entry_device(&device), device.as_str());
        assert!(entry_tag_counter(&device).starts_with("tag:"));
    }
}
