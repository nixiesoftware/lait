//! The OPFS pool's per-file header: 64 medium-owned bytes naming which
//! logical slot a physical file currently carries, if any.
//!
//! A pooled physical file outlives every logical slot it hosts, and the
//! header is what stops one life leaking into the next: the name says who
//! the bytes belong to, and its digest says the name itself was written
//! whole — a torn header reads as a spare, never as somebody's slot. Pure
//! bytes-in, bytes-out so the protocol is provable on any target; the OPFS
//! module owns the I/O around it.

/// magic 8 | name_len u16 | name (zero-padded) | digest 16.
pub(crate) const POOL_HEADER_LEN: usize = 64;
const POOL_MAGIC: &[u8; 8] = b"laitpool";
const DIGEST_LEN: usize = 16;
pub(crate) const POOL_NAME_CAPACITY: usize = POOL_HEADER_LEN - 8 - 2 - DIGEST_LEN;

fn digest(name: &[u8]) -> [u8; DIGEST_LEN] {
    let mut out = [0u8; DIGEST_LEN];
    if let Some(head) = blake3::hash(name).as_bytes().get(..DIGEST_LEN) {
        out.copy_from_slice(head);
    }
    out
}

/// Encode a header. `None` is a spare — a file waiting for its next life.
pub(crate) fn encode(name: Option<&str>) -> Option<[u8; POOL_HEADER_LEN]> {
    let bytes = name.map_or(&[][..], str::as_bytes);
    if bytes.len() > POOL_NAME_CAPACITY {
        return None;
    }
    let mut header = [0u8; POOL_HEADER_LEN];
    let (magic_part, rest) = header.split_at_mut(POOL_MAGIC.len());
    magic_part.copy_from_slice(POOL_MAGIC);
    let (len_part, rest) = rest.split_at_mut(2);
    len_part.copy_from_slice(&u16::try_from(bytes.len()).ok()?.to_le_bytes());
    let (name_part, digest_part) = rest.split_at_mut(POOL_NAME_CAPACITY);
    name_part.get_mut(..bytes.len())?.copy_from_slice(bytes);
    digest_part.copy_from_slice(&digest(bytes));
    Some(header)
}

/// Decode a header: `Ok(Some(name))` for an assigned file, `Ok(None)` for a
/// spare, `Err(())` for anything torn or foreign — which the pool treats as
/// a spare after re-establishing the header, never as a slot.
#[allow(
    clippy::result_unit_err,
    reason = "the one caller re-spares on any defect"
)]
pub(crate) fn decode(header: &[u8]) -> Result<Option<String>, ()> {
    let (magic, rest) = header.split_at_checked(POOL_MAGIC.len()).ok_or(())?;
    if magic != POOL_MAGIC {
        return Err(());
    }
    let (len_bytes, rest) = rest.split_at_checked(2).ok_or(())?;
    let name_len = usize::from(u16::from_le_bytes(
        <[u8; 2]>::try_from(len_bytes).map_err(|_| ())?,
    ));
    if name_len > POOL_NAME_CAPACITY {
        return Err(());
    }
    let (name_part, digest_part) = rest.split_at_checked(POOL_NAME_CAPACITY).ok_or(())?;
    let name = name_part.get(..name_len).ok_or(())?;
    if digest_part.get(..DIGEST_LEN) != Some(&digest(name)) {
        return Err(());
    }
    if name.is_empty() {
        return Ok(None);
    }
    String::from_utf8(name.to_vec()).map(Some).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_round_trips_assigned_and_spare() {
        let assigned = encode(Some("hot-17")).unwrap();
        assert_eq!(decode(&assigned), Ok(Some("hot-17".to_owned())));
        let spare = encode(None).unwrap();
        assert_eq!(decode(&spare), Ok(None));
        assert!(encode(Some(&"x".repeat(POOL_NAME_CAPACITY + 1))).is_none());
    }

    #[test]
    fn a_torn_header_is_a_spare_candidate_never_a_slot() {
        let mut torn = encode(Some("hot-3")).unwrap();
        // A name byte flipped after the digest was computed: the write tore.
        torn[11] ^= 0xFF;
        assert_eq!(decode(&torn), Err(()));
        // Foreign bytes and short bytes are equally not headers.
        assert_eq!(decode(&[0xAB; POOL_HEADER_LEN]), Err(()));
        assert_eq!(decode(&[0xAB; 10]), Err(()));
    }
}
