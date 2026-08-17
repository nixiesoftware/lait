//! Shared canonical-wire helpers for signed runtime envelopes.
//!
//! Contact's Body frame remains a byte-oriented chunk stream. The bytes are a
//! Replica-owned protected-artifact delivery pack; its signed `ArtifactRef`
//! closure is verified below the runtime boundary. Keeping that interpretation
//! out of this module lets Contact relay an opaque pack without keys while
//! humans and agents converge through the same Replica transaction primitive.

/// Build the length-framed signature preimage shared by every signed runtime
/// envelope: `u16be(domain_len) || domain || u32be(body_len) || body`. The
/// domain separates use-sites; the explicit lengths make the framing
/// unambiguous and canonical.
pub(crate) fn length_framed(domain: &[u8], body: &[u8]) -> Vec<u8> {
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
