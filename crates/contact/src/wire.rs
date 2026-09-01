//! The canonical length-framed signature preimage shared by every signed
//! runtime envelope: `u16be(domain_len) || domain || u32be(body_len) || body`.
//! The domain separates use-sites; the explicit lengths make the framing
//! unambiguous and canonical. Lives here because Contact's signatures are the
//! seam that crosses to wasm; the runtime re-exports it for its other
//! envelopes (presence, beacon).

pub fn length_framed(domain: &[u8], body: &[u8]) -> Vec<u8> {
    let capacity = 6usize
        .saturating_add(domain.len())
        .saturating_add(body.len());
    let mut out = Vec::with_capacity(capacity);
    let domain_len = u16::try_from(domain.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&domain_len.to_be_bytes());
    out.extend_from_slice(domain);
    let body_len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&body_len.to_be_bytes());
    out.extend_from_slice(body);
    out
}
