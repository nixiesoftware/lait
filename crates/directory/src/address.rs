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

    /// Mint one address from 16 bytes the *service* drew.
    ///
    /// Entropy is passed in rather than drawn here for the reason
    /// `crypto::DrawnEntropy` gives: a function that reaches for randomness
    /// cannot be tested against a known value, and the one property worth
    /// testing about minting is that a given draw always spells the same
    /// address.
    ///
    /// Word indices take 11 bits each, which is exact for a 2048-entry list and
    /// so unbiased. The numeric tail is a modulo, whose bias against a 64-bit
    /// draw is about 2^-50 — negligible, and negligible in the direction that
    /// does not matter anyway: an address is a *locator*, not a secret, and its
    /// sparseness argument is about occupancy rather than unpredictability.
    #[must_use]
    pub fn mint(entropy: &[u8; 16]) -> Self {
        let mut drawn = u128::from_be_bytes(*entropy);
        let mut words = [""; WORDS];
        for word in &mut words {
            // 2048 = 2^11, so masking is a uniform draw over the list.
            *word = crate::words::WORDS[(drawn & 0x7FF) as usize];
            drawn >>= 11;
        }
        let number = (drawn % u128::from(NUMBER_RANGE)) as u32;
        Self(format!(
            "{}-{}-{}-{number:04}",
            words[0], words[1], words[2]
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether every word is one this build would mint.
    ///
    /// Deliberately **not** part of [`Address::parse`]. Membership is a
    /// minting-time question: an address issued under this word list has to keep
    /// parsing if the list is ever revised, and a parser that rejected a word
    /// retired from a later edition would break addresses already spoken aloud
    /// and written on cards. So this exists for the minter to check its own
    /// output, and for a test to prove the two agree.
    #[must_use]
    pub fn is_mintable(&self) -> bool {
        self.0
            .split('-')
            .take(WORDS)
            .all(|word| crate::words::WORDS.binary_search(&word).is_ok())
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

    /// One draw always spells one address — the property that lets a mint be
    /// retried on collision without wondering whether it moved.
    #[test]
    fn a_draw_mints_the_same_address_every_time_and_parses_as_one() {
        let minted = Address::mint(&[0x5A; 16]);
        assert_eq!(minted, Address::mint(&[0x5A; 16]));
        let reparsed = Address::parse(minted.as_str()).expect("a minted address is canonical");
        assert_eq!(reparsed, minted);
        assert!(minted.is_mintable(), "{minted} uses words off the list");
    }

    /// Minting and parsing have to agree over the whole space, not at one point.
    /// A draw that produced a spelling the parser refused would be an address
    /// the service could issue and nobody could type back.
    #[test]
    fn every_minted_address_parses_and_uses_only_listed_words() {
        for seed in 0u8..=255 {
            let minted = Address::mint(&[seed; 16]);
            assert!(
                Address::parse(minted.as_str()).is_ok(),
                "{minted} was minted and does not parse"
            );
            assert!(minted.is_mintable(), "{minted} uses words off the list");
        }
    }

    /// The example the design Spec spells is not one this build would mint —
    /// `tin` and `quiet` are not on the list. That is not a defect in either:
    /// parsing checks shape so an address outlives a revision of the list, and
    /// this test is here so the difference is a recorded decision rather than a
    /// discrepancy somebody trips over.
    #[test]
    fn an_address_off_the_word_list_still_parses_and_says_it_is_not_mintable() {
        let address = Address::parse("tin-harbor-quiet-4417").expect("well formed");
        assert!(!address.is_mintable());
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
