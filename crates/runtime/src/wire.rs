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
pub(crate) use ::contact::wire::length_framed;
