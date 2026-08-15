//! Frozen encodings. Changing a byte here is a schema bump.

use data_encoding::HEXLOWER;
use serde::{Deserialize, Serialize};

use mechanics::ids::{ActorId, DeviceId, SpaceId};

use crate::ids::{CardId, PathHash};
use crate::types::{Evidence, GroupLink, Handle, HandleKey, Link, Stamp};
use crate::Error;

/// Schema this build writes.
pub const SCHEMA_VERSION: u8 = 1;

/// Handle-key encoding version, tagged inside the key.
pub const HANDLE_KEY_VERSION: u8 = 1;

#[derive(Serialize, Deserialize)]
enum HandleWire {
    V1Device(String),
    V1Actor { space: String, actor: String },
    V1Local { store: String, name: String },
}

/// Encode a handle as the frozen v1 key.
pub fn handle_key(handle: &Handle) -> Result<HandleKey, Error> {
    let wire = match handle {
        Handle::Device(id) => HandleWire::V1Device(id.as_str().to_owned()),
        Handle::Actor { space, actor } => HandleWire::V1Actor {
            space: space.as_str().to_owned(),
            actor: actor.as_str().to_owned(),
        },
        Handle::LocalAgent { store, name } => HandleWire::V1Local {
            store: store.as_str().to_owned(),
            name: name.clone(),
        },
    };
    let bytes = postcard::to_stdvec(&(HANDLE_KEY_VERSION, wire))
        .map_err(|_| Error::Invalid("handle key would not encode"))?;
    Ok(HandleKey::from_encoded(HEXLOWER.encode(&bytes)))
}

/// Decode a frozen handle key.
pub fn handle_from_key(key: &HandleKey) -> Result<Handle, Error> {
    let bytes = HEXLOWER
        .decode(key.as_str().as_bytes())
        .map_err(|_| Error::Invalid("handle key is not hex"))?;
    let (version, wire): (u8, HandleWire) =
        postcard::from_bytes(&bytes).map_err(|_| Error::Invalid("handle key is not v1"))?;
    if version != HANDLE_KEY_VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    match wire {
        HandleWire::V1Device(raw) => {
            let id = DeviceId::parse(&raw).ok_or(Error::Invalid("device handle"))?;
            Ok(Handle::Device(id))
        }
        HandleWire::V1Actor { space, actor } => {
            let space = SpaceId::parse(&space).ok_or(Error::Invalid("actor space"))?;
            let actor = ActorId::parse(&actor).ok_or(Error::Invalid("actor handle"))?;
            Ok(Handle::Actor { space, actor })
        }
        HandleWire::V1Local { store, name } => {
            let store = PathHash::parse(&store).ok_or(Error::Invalid("local-agent store"))?;
            if name.is_empty() {
                return Err(Error::Invalid("local-agent name"));
            }
            Ok(Handle::LocalAgent { store, name })
        }
    }
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    postcard::to_stdvec(value).map_err(|_| Error::Invalid("value would not encode"))
}

pub fn decode<'de, T: Deserialize<'de>>(bytes: &'de [u8]) -> Result<T, Error> {
    postcard::from_bytes(bytes).map_err(|_| Error::Corrupt("value would not decode"))
}

pub fn encode_stamp(stamp: &Stamp) -> Result<Vec<u8>, Error> {
    encode(stamp)
}

pub fn decode_stamp(bytes: &[u8]) -> Result<Stamp, Error> {
    decode(bytes)
}

pub fn encode_link(link: &Link) -> Result<Vec<u8>, Error> {
    encode(link)
}

pub fn decode_link(bytes: &[u8]) -> Result<Link, Error> {
    decode(bytes)
}

pub fn encode_group(link: &GroupLink) -> Result<Vec<u8>, Error> {
    encode(link)
}

pub fn decode_group(bytes: &[u8]) -> Result<GroupLink, Error> {
    decode(bytes)
}

pub fn encode_redirect(to: &CardId, stamp: &Stamp) -> Result<Vec<u8>, Error> {
    encode(&(to.as_str().to_owned(), stamp))
}

pub fn decode_redirect(bytes: &[u8]) -> Result<(CardId, Stamp), Error> {
    let (to, stamp): (String, Stamp) = decode(bytes)?;
    let to = CardId::parse(&to).ok_or(Error::Corrupt("redirect target"))?;
    Ok((to, stamp))
}

pub fn encode_u64(value: u64) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

pub fn decode_u64(bytes: &[u8]) -> Result<u64, Error> {
    let arr: [u8; 8] = bytes.try_into().map_err(|_| Error::Corrupt("u64 field"))?;
    Ok(u64::from_le_bytes(arr))
}

/// Evidence is never `Derived`; refuse it if a future writer invents one.
pub fn evidence_is_authored(evidence: &Evidence) -> bool {
    matches!(evidence, Evidence::Declared | Evidence::Asserted { .. })
}

/// Build the stored picture form (`<mime>;base64,<data>`) from raw image
/// bytes: sniff the format from the magic bytes, refuse what no client could
/// draw, and bound the stored size. The one constructor, so every stored
/// picture is drawable and every client sniffs nothing.
pub fn encode_picture(bytes: &[u8]) -> Result<String, Error> {
    let mime = match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        _ => return Err(Error::Invalid("a picture is a PNG, JPEG, or WebP file")),
    };
    let stored = format!("{mime};base64,{}", data_encoding::BASE64.encode(bytes));
    if stored.len() > crate::bounds::MAX_PICTURE_BYTES {
        return Err(Error::Bound("MAX_PICTURE_BYTES"));
    }
    Ok(stored)
}
