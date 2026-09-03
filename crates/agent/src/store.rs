//! Crash-safe private local storage under one agent identity home.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt as _;
use serde::{Deserialize, Serialize};

use crate::{AgentState, Error, OwnerAuthor, StateMutation, StateRevision};

const MAGIC: &[u8; 8] = b"LAITAGT1";
const ENVELOPE_VERSION: u8 = 1;
const STATE_VERSION: u16 = 1;
const PREFIX: usize = 8 + 1 + 4;
pub(crate) const MAX_STATE_BYTES: usize = 2 * 1024 * 1024;
const MAX_WRAPPED_OVERHEAD: u64 = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredState {
    version: u16,
    state: AgentState,
}

/// One private store rooted at `<agent identity home>/agent/state.bin`.
pub struct AgentStore {
    dir: PathBuf,
    state: PathBuf,
    temporary: PathBuf,
    lock: PathBuf,
}

impl AgentStore {
    #[must_use]
    pub fn at(identity_home: &Path) -> Self {
        let dir = identity_home.join("agent");
        Self {
            state: dir.join("state.bin"),
            temporary: dir.join("state.tmp"),
            lock: dir.join("state.lock"),
            dir,
        }
    }

    pub fn load(&self) -> Result<Option<AgentState>, Error> {
        self.prepare()?;
        let lock = self.lock()?;
        let result = self.load_unlocked();
        drop(lock);
        result
    }

    pub fn create(
        &self,
        state: &AgentState,
        agent_devices: &[mechanics::ids::DeviceId],
        owner_devices: &[mechanics::ids::DeviceId],
    ) -> Result<(), Error> {
        state.verify(agent_devices, owner_devices)?;
        self.prepare()?;
        let lock = self.lock()?;
        if self.state.exists() || self.temporary.exists() {
            drop(lock);
            return Err(Error::Invalid("agent state already exists"));
        }
        let result = self.write_unlocked(state);
        drop(lock);
        result
    }

    /// Serialize read-check-mutate-write behind the store lock. Authority and
    /// expected revisions are checked by the same domain mutation path used by
    /// in-memory callers.
    pub fn mutate(
        &self,
        author: &OwnerAuthor<'_>,
        expected: StateRevision,
        mutation: StateMutation,
    ) -> Result<AgentState, Error> {
        self.prepare()?;
        let lock = self.lock()?;
        let result = (|| {
            let mut held = self
                .load_unlocked()?
                .ok_or(Error::Invalid("agent state does not exist"))?;
            held.apply(author, expected, mutation)?;
            self.write_unlocked(&held)?;
            Ok(held)
        })();
        drop(lock);
        result
    }

    fn prepare(&self) -> Result<(), Error> {
        mechanics::secretfs::create_private_dir(&self.dir)
            .map_err(|error| Error::Storage(error.to_string()))
    }

    fn lock(&self) -> Result<File, Error> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock)?;
        file.lock_exclusive()?;
        Ok(file)
    }

    fn load_unlocked(&self) -> Result<Option<AgentState>, Error> {
        if self.state.exists() {
            return read_path(&self.state).map(Some);
        }
        if self.temporary.exists() {
            let recovered = read_path(&self.temporary)?;
            mechanics::secretfs::persist_replace(&self.temporary, &self.state)?;
            return Ok(Some(recovered));
        }
        Ok(None)
    }

    fn write_unlocked(&self, state: &AgentState) -> Result<(), Error> {
        let bytes = encode(state)?;
        mechanics::secretfs::write_private(
            &self.temporary,
            &bytes,
            mechanics::secretfs::Create::Replace,
            mechanics::secretfs::Wrap::Portable,
        )
        .map_err(|error| Error::Storage(error.to_string()))?;
        // Verify exactly what reached disk before replacing the standing state.
        let reread = read_path(&self.temporary)?;
        if &reread != state {
            return Err(Error::Corrupt("temporary state changed while writing"));
        }
        mechanics::secretfs::persist_replace(&self.temporary, &self.state)?;
        Ok(())
    }
}

fn read_path(path: &Path) -> Result<AgentState, Error> {
    let metadata = fs::metadata(path)?;
    let max = u64::try_from(MAX_STATE_BYTES)
        .map_err(|_| Error::Bound("agent state envelope"))?
        .checked_add(MAX_WRAPPED_OVERHEAD)
        .ok_or(Error::Bound("agent state envelope"))?;
    if metadata.len() > max {
        return Err(Error::Bound("agent state envelope"));
    }
    let bytes = mechanics::secretfs::read_private(path)
        .map_err(|error| Error::Storage(error.to_string()))?
        .ok_or(Error::Corrupt("state disappeared while reading"))?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(Error::Bound("agent state envelope"));
    }
    decode(&bytes)
}

fn encode(state: &AgentState) -> Result<Vec<u8>, Error> {
    state.validate()?;
    let stored = StoredState {
        version: STATE_VERSION,
        state: state.clone(),
    };
    let body = postcard::to_stdvec(&stored).map_err(|_| Error::Corrupt("state encode"))?;
    let body_len = u32::try_from(body.len()).map_err(|_| Error::Bound("agent state body"))?;
    let total = PREFIX
        .checked_add(body.len())
        .and_then(|len| len.checked_add(32))
        .ok_or(Error::Bound("agent state envelope"))?;
    if total > MAX_STATE_BYTES {
        return Err(Error::Bound("agent state envelope"));
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(MAGIC);
    out.push(ENVELOPE_VERSION);
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(blake3::hash(&out).as_bytes());
    Ok(out)
}

fn decode(bytes: &[u8]) -> Result<AgentState, Error> {
    if bytes.len() < PREFIX + 32 {
        return Err(Error::Corrupt("truncated envelope"));
    }
    if bytes.get(..8) != Some(MAGIC.as_slice()) {
        return Err(Error::Corrupt("envelope magic"));
    }
    let version = *bytes.get(8).ok_or(Error::Corrupt("envelope version"))?;
    if version != ENVELOPE_VERSION {
        return Err(Error::UnsupportedVersion {
            artifact: "agent state envelope",
            found: u16::from(version),
        });
    }
    let length: [u8; 4] = bytes
        .get(9..13)
        .and_then(|part| part.try_into().ok())
        .ok_or(Error::Corrupt("envelope length"))?;
    let body_len = usize::try_from(u32::from_le_bytes(length))
        .map_err(|_| Error::Corrupt("envelope length"))?;
    let body_end = PREFIX
        .checked_add(body_len)
        .ok_or(Error::Corrupt("envelope length"))?;
    let expected_len = body_end
        .checked_add(32)
        .ok_or(Error::Corrupt("envelope length"))?;
    if bytes.len() != expected_len {
        return Err(Error::Corrupt("envelope length disagrees with file"));
    }
    let digest: [u8; 32] = bytes
        .get(body_end..expected_len)
        .and_then(|part| part.try_into().ok())
        .ok_or(Error::Corrupt("envelope digest"))?;
    let signed = bytes
        .get(..body_end)
        .ok_or(Error::Corrupt("envelope digest"))?;
    if blake3::hash(signed) != digest {
        return Err(Error::Corrupt("envelope digest"));
    }
    let body = bytes
        .get(PREFIX..body_end)
        .ok_or(Error::Corrupt("envelope body"))?;
    let stored: StoredState =
        postcard::from_bytes(body).map_err(|_| Error::Corrupt("state decode"))?;
    if stored.version != STATE_VERSION {
        return Err(Error::UnsupportedVersion {
            artifact: "stored agent state",
            found: stored.version,
        });
    }
    stored.state.validate()?;
    Ok(stored.state)
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::{AgentState, Error};

    pub(crate) const ENVELOPE_VERSION: u8 = super::ENVELOPE_VERSION;

    pub(crate) fn encode(state: &AgentState) -> Result<Vec<u8>, Error> {
        super::encode(state)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<AgentState, Error> {
        super::decode(bytes)
    }
}
