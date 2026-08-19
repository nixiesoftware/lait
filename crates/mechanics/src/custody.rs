//! Portable custody for an authority share.
//!
//! # Why this exists
//!
//! A share protected only by DPAPI is bound to one Windows account on one
//! machine. For an N-of-N arrangement every share is indispensable, so losing a
//! profile does not degrade the group — it destroys the authority permanently.
//! That makes the operating-system profile an accidental founder, which nobody
//! chose and nobody can audit.
//!
//! So DPAPI is treated here as a **local convenience unlock**, never as the
//! durability boundary. The canonical artifact is an [`Package`]:
//! self-describing, portable, and openable by any of several independent
//! [`KeySlot`]s. Losing one slot costs convenience; it does not cost the share.
//!
//! # Shape
//!
//! One random data-encryption key encrypts the payload once. Each slot wraps
//! that DEK a different way, so adding an unlock path never re-encrypts the
//! secret and never requires having all paths present at once.
//!
//! The package binds itself to its context — space, authority, ceremony,
//! principal and leaf — so a restored share cannot be silently reopened against
//! the wrong space or mistaken for a different holder's. [`SharePayload`] is
//! an enum rather than raw bytes so the same envelope can carry a general-access
//! share without a format change.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::authority::{Authority, Holder, Principal, Scheme};
use crate::crypto::{self, SpaceKey};
use crate::ids::{DeviceId, SpaceId};

/// Current package format version.
pub const PACKAGE_VERSION: u16 = 1;

/// Argon2id parameters for a passphrase slot.
///
/// Stored in the package rather than assumed, so a package written today still
/// opens after the defaults are raised — a share that survives a decade must not
/// depend on the reader agreeing with the writer about cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Argon2Params {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for Argon2Params {
    /// RFC 9106's second recommended option (64 MiB, 3 passes), a reasonable
    /// interactive cost that still makes offline guessing expensive.
    fn default() -> Self {
        Argon2Params {
            m_cost_kib: 65536,
            t_cost: 3,
            p_cost: 1,
        }
    }
}

/// One way to unwrap the package's data-encryption key.
///
/// Slots are independent by construction: each wraps the same DEK, so any one of
/// them opens the package and losing any one of them costs nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeySlot {
    /// Unwrapped by the OS user-bound facility. Convenience only — this slot is
    /// worthless on any other account or machine, which is exactly why it must
    /// never be the only slot on an indispensable share.
    WindowsDpapi { wrapped_dek: Vec<u8> },
    /// Unwrapped by an x25519 keypair the custodian controls — a recovery key
    /// held offline, or another device.
    RecoveryKey {
        recipient: DeviceId,
        wrapped_dek: Vec<u8>,
    },
    /// Unwrapped by a passphrase the custodian remembers.
    Passphrase {
        salt: [u8; 16],
        params: Argon2Params,
        wrapped_dek: Vec<u8>,
    },
}

impl KeySlot {
    /// A short, stable label for status output.
    pub fn kind(&self) -> &'static str {
        match self {
            KeySlot::WindowsDpapi { .. } => "windows-dpapi",
            KeySlot::RecoveryKey { .. } => "recovery-key",
            KeySlot::Passphrase { .. } => "passphrase",
        }
    }
    /// Whether this slot can open the package away from the machine that wrote
    /// it. A package with no portable slot is one profile loss from gone.
    pub fn is_portable(&self) -> bool {
        !matches!(self, KeySlot::WindowsDpapi { .. })
    }
}

/// The secret a package carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SharePayload {
    Frost(FrostSharePayload),
    /// Reserved for the general-access backend.
    GeneralAccess(Vec<u8>),
}

/// A flat-FROST holder's private material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrostSharePayload {
    /// The serialized FROST key package.
    pub key_share: Vec<u8>,
    /// The public-key package, so a restored holder can derive the group key
    /// without needing anything else to have survived alongside it.
    pub public_package: Vec<u8>,
    /// This holder's 1-based participant index.
    pub index: u16,
}

/// A portable, self-describing custody envelope for one holder's share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub version: u16,
    pub space: SpaceId,
    pub authority: Authority,
    pub ceremony: String,
    pub scheme: Scheme,
    pub principal: Principal,
    pub leaf: Holder,
    /// [`SharePayload`], AEAD-encrypted under the package DEK.
    pub encrypted_payload: Vec<u8>,
    pub key_slots: Vec<KeySlot>,
}

/// What a package must match to be accepted after a restore.
///
/// Verification is a *comparison against expectations*, never a report of what
/// the package says about itself: a package that names its own space proves
/// nothing, and accepting one because its fields are internally consistent is
/// how a share for the wrong space, or another holder's share, gets adopted.
#[derive(Debug, Clone)]
pub struct PackageExpectation<'a> {
    pub space: &'a SpaceId,
    pub authority: &'a Authority,
    pub ceremony: &'a str,
    pub leaf: &'a Holder,
    /// The group public key this holder expects to be part of.
    pub group_key: &'a DeviceId,
    /// The participant index this holder expects to occupy.
    pub index: u16,
}

impl Package {
    /// Build a package, encrypting `payload` under a fresh DEK wrapped by every
    /// slot in `slot_specs`.
    pub fn seal(
        space: &SpaceId,
        authority: &Authority,
        ceremony: &str,
        principal: &Principal,
        leaf: &Holder,
        payload: &SharePayload,
        slot_specs: &[SlotSpec],
    ) -> Result<Self> {
        if slot_specs.is_empty() {
            return Err(anyhow!("a share package needs at least one unlock slot"));
        }
        let dek = crypto::random_key().map_err(|failure| anyhow!("protection: {failure:?}"))?;
        let plaintext = postcard::to_stdvec(payload)?;
        let encrypted_payload = crypto::aead_encrypt(&dek, &plaintext)
            .map_err(|failure| anyhow!("protection: {failure:?}"))?;
        let key_slots = slot_specs
            .iter()
            .map(|spec| spec.wrap(&dek))
            .collect::<Result<Vec<_>>>()?;
        Ok(Package {
            version: PACKAGE_VERSION,
            space: space.clone(),
            authority: authority.clone(),
            ceremony: ceremony.to_string(),
            scheme: authority_scheme_of(payload),
            principal: principal.clone(),
            leaf: leaf.clone(),
            encrypted_payload,
            key_slots,
        })
    }

    /// Open the payload with one unlock method.
    pub fn open(&self, key: &UnlockKey) -> Result<SharePayload> {
        if self.version != PACKAGE_VERSION {
            return Err(anyhow!(
                "share package version {} is not supported by this build",
                self.version
            ));
        }
        let dek = self
            .key_slots
            .iter()
            .find_map(|slot| key.unwrap(slot))
            .ok_or_else(|| anyhow!("no slot in this package can be opened with that key"))?;
        let plaintext = crypto::aead_decrypt(&dek, &self.encrypted_payload)
            .ok_or_else(|| anyhow!("the package payload did not decrypt — wrong key or corrupt"))?;
        Ok(postcard::from_bytes(&plaintext)?)
    }

    /// Whether any slot survives leaving this machine.
    pub fn has_portable_slot(&self) -> bool {
        self.key_slots.iter().any(KeySlot::is_portable)
    }

    /// Open the package and confirm it is the one expected, returning the
    /// payload only if every binding matches.
    ///
    /// This is the check a custodian performs before an indispensable authority
    /// is installed. It deliberately verifies the *group key derived from the
    /// package's own public-key package* against the expected one, so a package
    /// cannot claim membership of a group it was not part of.
    pub fn verify_and_open(
        &self,
        key: &UnlockKey,
        expect: &PackageExpectation<'_>,
    ) -> Result<SharePayload> {
        if &self.space != expect.space {
            return Err(anyhow!("this package belongs to a different space"));
        }
        if &self.authority != expect.authority {
            return Err(anyhow!("this package belongs to a different authority"));
        }
        if self.ceremony != expect.ceremony {
            return Err(anyhow!("this package belongs to a different ceremony"));
        }
        if &self.leaf != expect.leaf {
            return Err(anyhow!("this package belongs to a different holder"));
        }
        let payload = self.open(key)?;
        match &payload {
            SharePayload::Frost(f) => {
                let derived = crate::dkg::group_key_of_package(&f.public_package)
                    .map_err(|e| anyhow!("the package's public-key package is unusable: {e}"))?;
                if &derived != expect.group_key {
                    return Err(anyhow!(
                        "the package's own public-key package derives a different group key"
                    ));
                }
                if f.index != expect.index {
                    return Err(anyhow!(
                        "this package is for participant {}, not {}",
                        f.index,
                        expect.index
                    ));
                }
                // The decisive check: the PRIVATE material must actually work.
                // Everything above validates the public half, which a corrupted
                // or substituted secret would pass unchanged.
                crate::dkg::validate_share(&f.key_share, &f.public_package, f.index)?;
            }
            SharePayload::GeneralAccess(_) => {
                return Err(anyhow!(
                    "general-access share payloads are not supported by this build"
                ))
            }
        }
        Ok(payload)
    }
}

fn authority_scheme_of(payload: &SharePayload) -> Scheme {
    match payload {
        SharePayload::Frost(_) => Scheme::FrostThreshold,
        SharePayload::GeneralAccess(_) => Scheme::GeneralAccess,
    }
}

/// How to create one slot.
#[derive(Debug, Clone)]
pub enum SlotSpec {
    /// The caller supplies already-DPAPI-wrapped bytes; this crate does not know
    /// about the OS facility.
    WindowsDpapi {
        wrapped_dek: Vec<u8>,
    },
    RecoveryKey {
        recipient: DeviceId,
    },
    Passphrase {
        passphrase: String,
        salt: [u8; 16],
        params: Argon2Params,
    },
}

impl SlotSpec {
    fn wrap(&self, dek: &SpaceKey) -> Result<KeySlot> {
        match self {
            SlotSpec::WindowsDpapi { wrapped_dek } => Ok(KeySlot::WindowsDpapi {
                wrapped_dek: wrapped_dek.clone(),
            }),
            SlotSpec::RecoveryKey { recipient } => {
                let wrapped_dek = crypto::seal_to(recipient, dek.as_slice())
                    .map_err(|failure| anyhow!("protection: {failure:?}"))?
                    .ok_or_else(|| anyhow!("cannot seal to {}", recipient.short()))?;
                Ok(KeySlot::RecoveryKey {
                    recipient: recipient.clone(),
                    wrapped_dek,
                })
            }
            SlotSpec::Passphrase {
                passphrase,
                salt,
                params,
            } => {
                let kek = derive_passphrase_key(passphrase, salt, params)?;
                Ok(KeySlot::Passphrase {
                    salt: *salt,
                    params: *params,
                    wrapped_dek: crypto::aead_encrypt(&kek, dek.as_slice())
                        .map_err(|failure| anyhow!("protection: {failure:?}"))?,
                })
            }
        }
    }
}

/// A key that may open one kind of slot.
#[derive(Debug, Clone)]
pub enum UnlockKey {
    /// The caller unwrapped a DPAPI slot itself and supplies the DEK.
    Dpapi {
        dek: Vec<u8>,
    },
    RecoveryKey {
        seed: [u8; 32],
        me: DeviceId,
    },
    Passphrase(String),
}

impl UnlockKey {
    fn unwrap(&self, slot: &KeySlot) -> Option<SpaceKey> {
        match (self, slot) {
            (UnlockKey::Dpapi { dek }, KeySlot::WindowsDpapi { .. }) => {
                SpaceKey::try_from(dek.as_slice()).ok()
            }
            (
                UnlockKey::RecoveryKey { seed, me },
                KeySlot::RecoveryKey {
                    recipient,
                    wrapped_dek,
                },
            ) if recipient == me => {
                let raw = crypto::open_sealed(seed, me, wrapped_dek)?;
                SpaceKey::try_from(raw.as_slice()).ok()
            }
            (
                UnlockKey::Passphrase(p),
                KeySlot::Passphrase {
                    salt,
                    params,
                    wrapped_dek,
                },
            ) => {
                let kek = derive_passphrase_key(p, salt, params).ok()?;
                let raw = crypto::aead_decrypt(&kek, wrapped_dek)?;
                SpaceKey::try_from(raw.as_slice()).ok()
            }
            _ => None,
        }
    }
}

/// Argon2id over the passphrase. Memory-hard on purpose: a share package is
/// meant to be carried and stored, so its passphrase slot must survive an
/// attacker who has the file and unlimited offline guesses.
/// Current custodied-secret format version.
pub const CUSTODIED_VERSION: u16 = 1;

/// Domain for the purpose-bound payload key.
const CUSTODIED_DOMAIN: &str = "lait/custodied/1";

/// A secret held under several independent unlock paths, bound to a purpose.
///
/// # Why this is not a [`Package`]
///
/// [`Package`] carries an *authority share*, and binds itself to a space, an
/// authority, a ceremony, a principal and a leaf — the comparisons
/// [`Package::verify_and_open`] makes before an indispensable share is
/// installed. A secret that is none of those things has nothing to compare, so
/// putting one in that envelope would mean inventing a space and a holder for
/// it, and every one of those checks would become theatre performed against
/// values the writer chose.
///
/// What generalises is the other half, and it is the half the module exists
/// for: **one data-encryption key, wrapped by several independent
/// [`KeySlot`]s**, so adding an unlock path re-encrypts nothing and losing one
/// costs convenience rather than the secret. Any secret whose only protection
/// is the operating-system profile has the accidental-founder problem this
/// module was written to name, whether or not it is an authority share.
///
/// # What it binds instead
///
/// A `purpose` string, mixed into the payload key rather than merely recorded
/// beside it. A secret sealed for one purpose does not decrypt under another,
/// so misfiling is a decrypt failure rather than a policy question — the same
/// property [`crypto::seal_to_bound`] buys by putting its context in `info`,
/// and the reason a caller cannot open a display coordinator's key by asking
/// for it under some other name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Custodied {
    pub version: u16,
    /// Recorded for diagnosis. It is *not* what makes opening safe — the key
    /// derivation is — so a reader that trusts this field alone has learned
    /// nothing an author could not have written.
    pub purpose: String,
    encrypted_payload: Vec<u8>,
    key_slots: Vec<KeySlot>,
}

impl Custodied {
    /// Seal `secret` under a fresh DEK wrapped by every slot in `slot_specs`.
    pub fn seal(purpose: &str, secret: &[u8], slot_specs: &[SlotSpec]) -> Result<Self> {
        if purpose.is_empty() {
            return Err(anyhow!("a custodied secret needs a purpose to bind to"));
        }
        if slot_specs.is_empty() {
            return Err(anyhow!("a custodied secret needs at least one unlock slot"));
        }
        let dek = crypto::random_key().map_err(|failure| anyhow!("protection: {failure:?}"))?;
        let encrypted_payload = crypto::aead_encrypt(&payload_key(purpose, &dek), secret)
            .map_err(|failure| anyhow!("protection: {failure:?}"))?;
        let key_slots = slot_specs
            .iter()
            .map(|spec| spec.wrap(&dek))
            .collect::<Result<Vec<_>>>()?;
        Ok(Custodied {
            version: CUSTODIED_VERSION,
            purpose: purpose.to_string(),
            encrypted_payload,
            key_slots,
        })
    }

    /// Open the secret with one unlock method, for the purpose the caller
    /// expects.
    ///
    /// `purpose` is the caller's *expectation*, not the envelope's claim: it is
    /// mixed into the key, so naming the wrong one fails to decrypt even though
    /// the slot opened.
    pub fn open(&self, purpose: &str, key: &UnlockKey) -> Result<Vec<u8>> {
        let dek = self.unwrap_dek(key)?;
        crypto::aead_decrypt(&payload_key(purpose, &dek), &self.encrypted_payload).ok_or_else(
            || {
                anyhow!(
                    "the custodied secret did not decrypt — wrong key, wrong purpose, or corrupt"
                )
            },
        )
    }

    /// Add an unlock path, proving one already held.
    ///
    /// The payload is untouched: only the DEK is re-wrapped, which is the whole
    /// reason slots are independent. Admitting a path that is already present
    /// is refused rather than silently duplicated, so a slot list stays a set of
    /// distinct answers to "who can open this".
    pub fn admit(&mut self, key: &UnlockKey, spec: &SlotSpec) -> Result<()> {
        let dek = self.unwrap_dek(key)?;
        let slot = spec.wrap(&dek)?;
        if self.key_slots.iter().any(|held| same_path(held, &slot)) {
            return Err(anyhow!(
                "this custodied secret already has a {} slot for that holder",
                slot.kind()
            ));
        }
        self.key_slots.push(slot);
        Ok(())
    }

    /// Whether any slot survives leaving this machine. A secret with no
    /// portable slot is one profile loss from gone.
    pub fn has_portable_slot(&self) -> bool {
        self.key_slots.iter().any(KeySlot::is_portable)
    }

    /// The unlock paths this secret holds, for status output.
    pub fn slot_kinds(&self) -> Vec<&'static str> {
        self.key_slots.iter().map(KeySlot::kind).collect()
    }

    fn unwrap_dek(&self, key: &UnlockKey) -> Result<SpaceKey> {
        if self.version != CUSTODIED_VERSION {
            return Err(anyhow!(
                "custodied secret version {} is not supported by this build",
                self.version
            ));
        }
        self.key_slots
            .iter()
            .find_map(|slot| key.unwrap(slot))
            .ok_or_else(|| anyhow!("no slot in this secret can be opened with that key"))
    }
}

/// Two slots are the same *path* when losing one would not leave the other —
/// which is per-holder for a recovery key and per-kind for the rest.
fn same_path(held: &KeySlot, candidate: &KeySlot) -> bool {
    match (held, candidate) {
        (KeySlot::RecoveryKey { recipient: a, .. }, KeySlot::RecoveryKey { recipient: b, .. }) => {
            a == b
        }
        (KeySlot::WindowsDpapi { .. }, KeySlot::WindowsDpapi { .. }) => true,
        (KeySlot::Passphrase { .. }, KeySlot::Passphrase { .. }) => true,
        _ => false,
    }
}

/// Bind the purpose into the key that encrypts the payload, so a secret sealed
/// for one purpose is undecryptable under another.
fn payload_key(purpose: &str, dek: &SpaceKey) -> SpaceKey {
    blake3::derive_key(&format!("{CUSTODIED_DOMAIN} {purpose}"), dek.as_slice())
}

fn derive_passphrase_key(
    passphrase: &str,
    salt: &[u8; 16],
    params: &Argon2Params,
) -> Result<SpaceKey> {
    let params = argon2::Params::new(params.m_cost_kib, params.t_cost, params.p_cost, Some(32))
        .map_err(|e| anyhow!("argon2 parameters rejected: {e}"))?;
    let a2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = [0u8; 32];
    a2.hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .map_err(|e| anyhow!("argon2 derivation failed: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Config, FrostThresholdConfig};
    use crate::ids::SystemUlidSource;

    /// Cheap parameters so tests do not spend a second per derivation. Never use
    /// these for a real package.
    fn fast() -> Argon2Params {
        Argon2Params {
            m_cost_kib: 64,
            t_cost: 1,
            p_cost: 1,
        }
    }

    fn fixture() -> (
        SpaceId,
        Authority,
        Principal,
        Holder,
        SharePayload,
        DeviceId,
    ) {
        let ws = SpaceId::mint(&SystemUlidSource);
        let (holders, group_key) = crate::dkg::tests_support::run_dkg(3, 2);
        let (share, pkp) = holders[&1].clone();
        let device = crypto::device_from_seed(&[1u8; 32]);
        let principal = Principal::of_device(&device);
        let leaf = Holder::of_principal(&principal);
        let config = Config::frost_threshold(&FrostThresholdConfig {
            k: 2,
            participants: vec![principal.clone()],
        });
        let authority = Authority::new(group_key.clone(), &config);
        let payload = SharePayload::Frost(FrostSharePayload {
            key_share: share,
            public_package: pkp,
            index: 1,
        });
        (ws, authority, principal, leaf, payload, group_key)
    }

    const IDENTIFIER: &str = "lait/display/identifier-key/1";

    #[test]
    fn a_custodied_secret_survives_losing_one_unlock_path() {
        let device = crypto::device_from_seed(&[9u8; 32]);
        let secret = [3u8; 32];
        let held = Custodied::seal(
            IDENTIFIER,
            &secret,
            &[
                SlotSpec::WindowsDpapi {
                    wrapped_dek: b"a profile-bound convenience".to_vec(),
                },
                SlotSpec::RecoveryKey {
                    recipient: device.clone(),
                },
            ],
        )
        .unwrap();

        // The profile is gone: its slot cannot be unwrapped, and the secret is
        // still here. That is the whole point of the split.
        assert_eq!(
            held.open(
                IDENTIFIER,
                &UnlockKey::RecoveryKey {
                    seed: [9u8; 32],
                    me: device,
                },
            )
            .unwrap(),
            secret
        );
        assert!(held.has_portable_slot());
    }

    #[test]
    fn a_secret_sealed_for_one_purpose_does_not_open_as_another() {
        let device = crypto::device_from_seed(&[11u8; 32]);
        let held = Custodied::seal(
            IDENTIFIER,
            &[5u8; 32],
            &[SlotSpec::RecoveryKey {
                recipient: device.clone(),
            }],
        )
        .unwrap();
        let key = UnlockKey::RecoveryKey {
            seed: [11u8; 32],
            me: device,
        };

        // The slot opens — the caller genuinely holds it — and the payload
        // still refuses, because the purpose is in the key rather than beside
        // it. A misfiling is a decrypt failure, not a policy question.
        assert!(held.open("lait/some-other-secret/1", &key).is_err());
        assert!(held.open(IDENTIFIER, &key).is_ok());
    }

    #[test]
    fn admitting_a_path_re_wraps_the_key_and_never_the_payload() {
        let first = crypto::device_from_seed(&[13u8; 32]);
        let second = crypto::device_from_seed(&[17u8; 32]);
        let secret = [23u8; 32];
        let mut held = Custodied::seal(
            IDENTIFIER,
            &secret,
            &[SlotSpec::RecoveryKey {
                recipient: first.clone(),
            }],
        )
        .unwrap();
        let ciphertext = held.encrypted_payload.clone();

        held.admit(
            &UnlockKey::RecoveryKey {
                seed: [13u8; 32],
                me: first,
            },
            &SlotSpec::RecoveryKey {
                recipient: second.clone(),
            },
        )
        .unwrap();

        assert_eq!(
            held.encrypted_payload, ciphertext,
            "admitting a reader re-encrypted the secret"
        );
        assert_eq!(
            held.open(
                IDENTIFIER,
                &UnlockKey::RecoveryKey {
                    seed: [17u8; 32],
                    me: second,
                },
            )
            .unwrap(),
            secret
        );
    }

    #[test]
    fn a_stranger_cannot_admit_a_path() {
        let holder = crypto::device_from_seed(&[29u8; 32]);
        let stranger = crypto::device_from_seed(&[31u8; 32]);
        let mut held = Custodied::seal(
            IDENTIFIER,
            &[37u8; 32],
            &[SlotSpec::RecoveryKey { recipient: holder }],
        )
        .unwrap();

        let refused = held.admit(
            &UnlockKey::RecoveryKey {
                seed: [31u8; 32],
                me: stranger.clone(),
            },
            &SlotSpec::RecoveryKey {
                recipient: stranger,
            },
        );
        assert!(refused.is_err(), "a stranger admitted itself");
        assert_eq!(held.slot_kinds(), vec!["recovery-key"]);
    }

    #[test]
    fn a_dpapi_only_secret_reports_itself_as_one_profile_from_gone() {
        let held = Custodied::seal(
            IDENTIFIER,
            &[41u8; 32],
            &[SlotSpec::WindowsDpapi {
                wrapped_dek: b"only this profile".to_vec(),
            }],
        )
        .unwrap();
        assert!(!held.has_portable_slot());
    }

    #[test]
    fn a_custodied_secret_needs_a_purpose_and_a_slot() {
        assert!(Custodied::seal("", &[1u8; 32], &[]).is_err());
        assert!(Custodied::seal(IDENTIFIER, &[1u8; 32], &[]).is_err());
    }

    #[test]
    fn a_passphrase_slot_opens_the_package_anywhere() {
        let (ws, authority, principal, leaf, payload, _) = fixture();
        let pkg = Package::seal(
            &ws,
            &authority,
            "ceremony-1",
            &principal,
            &leaf,
            &payload,
            &[SlotSpec::Passphrase {
                passphrase: "correct horse battery staple".into(),
                salt: [7u8; 16],
                params: fast(),
            }],
        )
        .unwrap();
        assert!(pkg.has_portable_slot());
        assert_eq!(
            pkg.open(&UnlockKey::Passphrase(
                "correct horse battery staple".into()
            ))
            .unwrap(),
            payload
        );
        assert!(pkg.open(&UnlockKey::Passphrase("wrong".into())).is_err());
    }

    /// Slots are independent: any one opens the package, so losing one costs
    /// convenience rather than the share. This is the property that stops a
    /// Windows profile from being an accidental founder.
    #[test]
    fn any_single_slot_opens_the_same_package() {
        let (ws, authority, principal, leaf, payload, _) = fixture();
        let device = crypto::device_from_seed(&[9u8; 32]);
        let pkg = Package::seal(
            &ws,
            &authority,
            "ceremony-1",
            &principal,
            &leaf,
            &payload,
            &[
                SlotSpec::WindowsDpapi {
                    wrapped_dek: vec![0u8; 4],
                },
                SlotSpec::RecoveryKey {
                    recipient: device.clone(),
                },
                SlotSpec::Passphrase {
                    passphrase: "pass".into(),
                    salt: [1u8; 16],
                    params: fast(),
                },
            ],
        )
        .unwrap();
        assert_eq!(pkg.key_slots.len(), 3);
        // The recovery-key slot alone.
        assert_eq!(
            pkg.open(&UnlockKey::RecoveryKey {
                seed: [9u8; 32],
                me: device,
            })
            .unwrap(),
            payload
        );
        // The passphrase slot alone.
        assert_eq!(
            pkg.open(&UnlockKey::Passphrase("pass".into())).unwrap(),
            payload
        );
    }

    /// A DPAPI-only package is exactly the failure this module exists to
    /// prevent: openable today, worthless on any other machine.
    #[test]
    fn a_dpapi_only_package_is_not_portable() {
        let (ws, authority, principal, leaf, payload, _) = fixture();
        let pkg = Package::seal(
            &ws,
            &authority,
            "ceremony-1",
            &principal,
            &leaf,
            &payload,
            &[SlotSpec::WindowsDpapi {
                wrapped_dek: vec![0u8; 4],
            }],
        )
        .unwrap();
        assert!(
            !pkg.has_portable_slot(),
            "a DPAPI-only share is one profile loss from gone"
        );
    }

    #[test]
    fn a_package_needs_at_least_one_slot() {
        let (ws, authority, principal, leaf, payload, _) = fixture();
        assert!(Package::seal(
            &ws,
            &authority,
            "ceremony-1",
            &principal,
            &leaf,
            &payload,
            &[]
        )
        .is_err());
    }

    /// Verification compares against expectations rather than reading the
    /// package's claims about itself.
    #[test]
    fn verification_rejects_a_package_from_the_wrong_context() {
        let (ws, authority, principal, leaf, payload, group_key) = fixture();
        let pkg = Package::seal(
            &ws,
            &authority,
            "ceremony-1",
            &principal,
            &leaf,
            &payload,
            &[SlotSpec::Passphrase {
                passphrase: "pass".into(),
                salt: [1u8; 16],
                params: fast(),
            }],
        )
        .unwrap();
        let key = UnlockKey::Passphrase("pass".into());
        let good = PackageExpectation {
            space: &ws,
            authority: &authority,
            ceremony: "ceremony-1",
            leaf: &leaf,
            group_key: &group_key,
            index: 1,
        };
        assert!(pkg.verify_and_open(&key, &good).is_ok());

        let other_ws = SpaceId::mint(&SystemUlidSource);
        assert!(pkg
            .verify_and_open(
                &key,
                &PackageExpectation {
                    space: &other_ws,
                    ..good.clone()
                }
            )
            .is_err());
        assert!(pkg
            .verify_and_open(
                &key,
                &PackageExpectation {
                    ceremony: "ceremony-2",
                    ..good.clone()
                }
            )
            .is_err());
        let other_leaf = Holder::of_principal(&Principal::of_device(&crypto::device_from_seed(
            &[42u8; 32],
        )));
        assert!(pkg
            .verify_and_open(
                &key,
                &PackageExpectation {
                    leaf: &other_leaf,
                    ..good.clone()
                }
            )
            .is_err());
        // And the group key is checked by DERIVING it from the package's own
        // public-key package, not by trusting a field.
        let other_key = crypto::device_from_seed(&[99u8; 32]);
        assert!(pkg
            .verify_and_open(
                &key,
                &PackageExpectation {
                    group_key: &other_key,
                    ..good
                }
            )
            .is_err());
    }

    /// A package whose private material does not work is refused, even though
    /// every public-half check passes and the envelope opens cleanly.
    ///
    /// This is the shape that would otherwise produce an honest custody
    /// attestation for a dead share — and, for an N-of-N arrangement, install an
    /// authority on it.
    #[test]
    fn a_package_with_unusable_private_material_is_refused() {
        let ws = SpaceId::mint(&SystemUlidSource);
        let (forged, pkp, group_key) = crate::dkg::tests_support::share_with_foreign_secret();
        let device = crypto::device_from_seed(&[1u8; 32]);
        let principal = Principal::of_device(&device);
        let leaf = Holder::of_principal(&principal);
        let config = Config::frost_threshold(&FrostThresholdConfig {
            k: 2,
            participants: vec![principal.clone()],
        });
        let authority = Authority::new(group_key.clone(), &config);
        let broken = SharePayload::Frost(FrostSharePayload {
            key_share: forged,
            public_package: pkp.clone(),
            index: 1,
        });
        let pkg = Package::seal(
            &ws,
            &authority,
            "ceremony-1",
            &principal,
            &leaf,
            &broken,
            &[SlotSpec::Passphrase {
                passphrase: "pass".into(),
                salt: [1u8; 16],
                params: fast(),
            }],
        )
        .unwrap();
        let key = UnlockKey::Passphrase("pass".into());
        // The public half is impeccable and the envelope opens.
        assert_eq!(crate::dkg::group_key_of_package(&pkp).unwrap(), group_key);
        assert!(pkg.open(&key).is_ok());
        // Verification refuses it anyway.
        let err = pkg
            .verify_and_open(
                &key,
                &PackageExpectation {
                    space: &ws,
                    authority: &authority,
                    ceremony: "ceremony-1",
                    leaf: &leaf,
                    group_key: &group_key,
                    index: 1,
                },
            )
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("does not correspond to its public half"),
            "must reject unusable private material: {err}"
        );
    }

    #[test]
    fn a_package_round_trips_through_its_wire_form() {
        let (ws, authority, principal, leaf, payload, _) = fixture();
        let pkg = Package::seal(
            &ws,
            &authority,
            "ceremony-1",
            &principal,
            &leaf,
            &payload,
            &[SlotSpec::Passphrase {
                passphrase: "pass".into(),
                salt: [1u8; 16],
                params: fast(),
            }],
        )
        .unwrap();
        let bytes = postcard::to_stdvec(&pkg).unwrap();
        let back: Package = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(pkg, back);
        assert_eq!(
            back.open(&UnlockKey::Passphrase("pass".into())).unwrap(),
            payload
        );
    }
}
