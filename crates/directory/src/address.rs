//! An issued address: short, speakable, and not chosen by its holder.
//!
//! Three words and a number — `tin-harbor-quiet-4417`. Issuance is what makes the rest
//! of the position hold. Nobody squats, because nobody picks. Nobody registers a
//! confusable, because nobody selects. And enumeration is defeated by sparseness
//! before any refusal is needed: the occupied fraction of the space is
//! negligible, so probing returns nothing at a rate worth having.
//!
//! **Skeleton.** The word list is not here — a real one is 2048 curated words,
//! and picking them (no homophones, no substrings of each other, no unfortunate
//! pairs) is its own task rather than something to improvise inline.

use crate::Refusal;

/// Words in the list a real deployment carries.
pub const WORD_COUNT: usize = 2048;

/// How many words an address uses.
pub const WORDS: usize = 3;

/// The numeric tail, exclusive.
pub const NUMBER_RANGE: u32 = 10_000;

/// The whole space: `2048^3` words (≈2^33) times the numeric tail, ≈2^46.
#[must_use]
pub const fn keyspace() -> u128 {
    (WORD_COUNT as u128).pow(WORDS as u32) * NUMBER_RANGE as u128
}

/// An address as issued.
///
/// Stored as its canonical text. Comparison is on that text: a spelling that is
/// not canonical is refused rather than normalised, the same rule the Post keeps
/// for a device id, and for the same reason — two spellings would be two
/// entries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address(String);

impl Address {
    /// Parse a canonical address. Refuses anything it does not fully understand.
    ///
    /// [`Refusal::Malformed`] rather than [`Refusal::NotAvailable`]: the oracle
    /// argument is about *resolution*, where two distinguishable answers tell a
    /// prober whether somebody exists. This is a local parse of what a person
    /// typed, facing no prober, and "you typed it wrong" and "no such address"
    /// are exactly the two facts that must not be folded together.
    pub fn parse(raw: &str) -> Result<Self, Refusal> {
        let trimmed = raw.trim();
        let mut parts = trimmed.split('-');
        let words: Vec<&str> = (&mut parts).take(WORDS).collect();
        let Some(number) = parts.next() else {
            return Err(Refusal::Malformed);
        };
        if parts.next().is_some() || words.len() != WORDS {
            return Err(Refusal::Malformed);
        }
        let well_formed = words
            .iter()
            .all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase()))
            && number.len() == 4
            && number.chars().all(|c| c.is_ascii_digit());
        if !well_formed {
            return Err(Refusal::Malformed);
        }
        Ok(Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_address_parses_and_keeps_its_spelling() {
        let address = Address::parse("tin-harbor-quiet-4417").expect("canonical");
        assert_eq!(address.as_str(), "tin-harbor-quiet-4417");
    }

    /// A non-canonical spelling is refused rather than repaired, so one address
    /// is never two entries.
    #[test]
    fn a_spelling_that_is_not_canonical_is_refused_rather_than_normalised() {
        for raw in [
            "Tin-Harbor-Quiet-4417",
            "tin-harbor-4417",
            "tin-harbor-quiet-441",
            "tin-harbor-quiet-44170",
            "tin-harbor-quiet-abcd",
            "tin_harbor_quiet_4417",
            "tin-harbor-quiet-4417-extra",
            "",
        ] {
            assert!(
                matches!(Address::parse(raw), Err(Refusal::Malformed)),
                "{raw} is not canonical, and says so rather than answering about existence"
            );
        }
    }

    /// The sparseness the no-enumeration position leans on, asserted rather than
    /// assumed: a prober walking this space finds nothing at a useful rate.
    /// Stated against occupancy, which is the property the no-enumeration
    /// position actually rests on: with a million addresses issued, a prober
    /// guessing at any rate worth having still finds essentially nothing.
    #[test]
    fn a_million_issued_addresses_still_cost_a_prober_millions_of_guesses_each() {
        const ISSUED: u128 = 1_000_000;
        let guesses_per_hit = keyspace() / ISSUED;
        assert!(
            guesses_per_hit > 10_000_000,
            "a prober needs {guesses_per_hit} guesses per hit against a keyspace of {}",
            keyspace()
        );
    }
}
