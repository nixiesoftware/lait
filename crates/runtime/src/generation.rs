//! Durable generations of an Orbit's derived material.
//!
//! Authority effects and Body transactions are durable facts. Their journal,
//! indexes, checkpoints, and manifests are representations of those facts.
//! This module gives an Orbit one activation point for a complete pair of
//! representations: Mechanics and Replica are built in isolation, verified by
//! their semantic owners, then selected together by one atomic pointer.

#[cfg(unix)]
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

const GENERATIONS_DIR: &str = "generations";
const ACTIVE_FILE: &str = "active-generation";
const ACTIVE_LOCK: &str = "active-generation.lock";
const POINTER_MAGIC: &[u8] = b"lait/orbit-generation/1";
const MAX_POINTER: usize = 512;

/// One immutable, complete materialization of an Orbit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Generation([u8; 32]);

impl Generation {
    /// Derive a generation identity from its predecessor and canonical recipe.
    /// The recipe should commit to every representation version and source
    /// root that controls the build.
    pub fn derive(source: Option<Self>, recipe: &[u8]) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(b"lait/orbit-generation/1/identity");
        match source {
            None => {
                hash.update(&[0]);
            }
            Some(source) => {
                hash.update(&[1]);
                hash.update(&source.0);
            }
        }
        let recipe_len = u64::try_from(recipe.len()).unwrap_or(u64::MAX);
        hash.update(&recipe_len.to_le_bytes());
        hash.update(recipe);
        Self(*hash.finalize().as_bytes())
    }

    pub fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub fn digest(self) -> [u8; 32] {
        self.0
    }

    fn directory(self) -> String {
        data_encoding::HEXLOWER.encode(&self.0)
    }
}

impl std::fmt::Display for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.directory())
    }
}

/// The two derived stores selected by the Orbit generation pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Replica,
    Mechanics,
}

impl Component {
    fn directory(self) -> &'static str {
        match self {
            Self::Replica => "replica",
            Self::Mechanics => "mechanics",
        }
    }
}

/// Semantic-equivalence evidence produced by the owners of the derived data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence([u8; 32]);

impl Evidence {
    pub fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub fn digest(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Io(std::io::ErrorKind),
    AlreadyExists,
    MissingComponent(Component),
    CorruptPointer,
    SourceChanged,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

fn io(error: std::io::Error) -> Error {
    Error::Io(error.kind())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Pointer {
    generation: Generation,
    source: Option<Generation>,
    evidence: Evidence,
    checksum: [u8; 32],
}

impl Pointer {
    fn new(generation: Generation, source: Option<Generation>, evidence: Evidence) -> Self {
        Self {
            generation,
            source,
            evidence,
            checksum: pointer_checksum(generation, source, evidence),
        }
    }

    fn encode(&self) -> Result<Vec<u8>, Error> {
        let body = postcard::to_stdvec(self).map_err(|_| Error::CorruptPointer)?;
        let mut bytes = Vec::with_capacity(POINTER_MAGIC.len().saturating_add(body.len()));
        bytes.extend_from_slice(POINTER_MAGIC);
        bytes.extend_from_slice(&body);
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_POINTER || !bytes.starts_with(POINTER_MAGIC) {
            return Err(Error::CorruptPointer);
        }
        let body = bytes
            .get(POINTER_MAGIC.len()..)
            .ok_or(Error::CorruptPointer)?;
        let pointer: Self = postcard::from_bytes(body).map_err(|_| Error::CorruptPointer)?;
        if pointer.checksum
            != pointer_checksum(pointer.generation, pointer.source, pointer.evidence)
            || pointer.encode()?.as_slice() != bytes
        {
            return Err(Error::CorruptPointer);
        }
        Ok(pointer)
    }
}

fn pointer_checksum(
    generation: Generation,
    source: Option<Generation>,
    evidence: Evidence,
) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"lait/orbit-generation/1/pointer");
    hash.update(&generation.0);
    match source {
        None => {
            hash.update(&[0]);
        }
        Some(source) => {
            hash.update(&[1]);
            hash.update(&source.0);
        }
    }
    hash.update(&evidence.0);
    *hash.finalize().as_bytes()
}

/// The generation an Orbit currently resolves to.
#[derive(Debug, Clone)]
pub struct Active {
    orbit: PathBuf,
    pointer: Option<Pointer>,
}

impl Active {
    pub fn read(orbit: impl AsRef<Path>) -> Result<Self, Error> {
        let orbit = orbit.as_ref().to_path_buf();
        let pointer = match std::fs::read(orbit.join(ACTIVE_FILE)) {
            Ok(bytes) => Some(Pointer::decode(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(io(error)),
        };
        Ok(Self { orbit, pointer })
    }

    /// `None` names the original implicit layout.
    pub fn generation(&self) -> Option<Generation> {
        self.pointer.as_ref().map(|pointer| pointer.generation)
    }

    pub fn evidence(&self) -> Option<Evidence> {
        self.pointer.as_ref().map(|pointer| pointer.evidence)
    }

    pub fn path(&self, component: Component) -> PathBuf {
        match self.generation() {
            None => match component {
                Component::Replica => self.orbit.clone(),
                Component::Mechanics => self.orbit.join("authority"),
            },
            Some(generation) => self
                .orbit
                .join(GENERATIONS_DIR)
                .join(generation.directory())
                .join(component.directory()),
        }
    }
}

/// An isolated construction of the next generation.
#[derive(Debug)]
pub struct Build {
    orbit: PathBuf,
    source: Option<Generation>,
    generation: Generation,
    staging: PathBuf,
    completed: PathBuf,
    sealed: bool,
}

impl Build {
    pub fn begin(orbit: impl AsRef<Path>, generation: Generation) -> Result<Self, Error> {
        let orbit = orbit.as_ref().to_path_buf();
        let source = Active::read(&orbit)?.generation();
        let generations = orbit.join(GENERATIONS_DIR);
        std::fs::create_dir_all(&generations).map_err(io)?;
        let name = generation.directory();
        let staging = generations.join(format!("{name}.building"));
        let completed = generations.join(name);
        if staging.exists() || completed.exists() {
            return Err(Error::AlreadyExists);
        }
        std::fs::create_dir(&staging).map_err(io)?;
        std::fs::create_dir(staging.join(Component::Replica.directory())).map_err(io)?;
        std::fs::create_dir(staging.join(Component::Mechanics.directory())).map_err(io)?;
        sync_dir(&generations).map_err(io)?;
        Ok(Self {
            orbit,
            source,
            generation,
            staging,
            completed,
            sealed: false,
        })
    }

    /// Begin again after an interrupted, unverified construction.
    ///
    /// The caller must hold the Orbit's operational lock. Only the exact
    /// deterministic `.building` directory is reclaimed; an immutable
    /// completed generation is never removed or overwritten.
    pub fn restart(orbit: impl AsRef<Path>, generation: Generation) -> Result<Self, Error> {
        let orbit = orbit.as_ref();
        let staging = orbit
            .join(GENERATIONS_DIR)
            .join(format!("{}.building", generation.directory()));
        match std::fs::symlink_metadata(&staging) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                std::fs::remove_dir_all(&staging).map_err(io)?;
                if let Some(generations) = staging.parent() {
                    sync_dir(generations).map_err(io)?;
                }
            }
            Ok(_) => return Err(Error::AlreadyExists),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(error)),
        }
        Self::begin(orbit, generation)
    }

    pub fn generation(&self) -> Generation {
        self.generation
    }

    pub fn source(&self) -> Option<Generation> {
        self.source
    }

    pub fn path(&self, component: Component) -> PathBuf {
        self.staging.join(component.directory())
    }

    /// Seal a complete build after its semantic owners have established the
    /// supplied equivalence evidence.
    pub fn verify(mut self, evidence: Evidence) -> Result<Verification, Error> {
        for component in [Component::Replica, Component::Mechanics] {
            let path = self.path(component);
            if !path.is_dir() {
                return Err(Error::MissingComponent(component));
            }
        }
        sync_tree(&self.staging)?;
        std::fs::rename(&self.staging, &self.completed).map_err(io)?;
        self.sealed = true;
        let generations = self
            .completed
            .parent()
            .ok_or(Error::Io(std::io::ErrorKind::InvalidInput))?;
        sync_dir(generations).map_err(io)?;
        Ok(Verification {
            orbit: self.orbit.clone(),
            source: self.source,
            generation: self.generation,
            evidence,
        })
    }
}

impl Drop for Build {
    fn drop(&mut self) {
        if !self.sealed {
            let _ = std::fs::remove_dir_all(&self.staging);
        }
    }
}

/// A complete generation accompanied by semantic-equivalence evidence.
#[derive(Debug)]
pub struct Verification {
    orbit: PathBuf,
    source: Option<Generation>,
    generation: Generation,
    evidence: Evidence,
}

impl Verification {
    /// Atomically select this generation if its source is still active.
    pub fn activate(self) -> Result<Activation, Error> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.orbit.join(ACTIVE_LOCK))
            .map_err(io)?;
        lock.lock_exclusive().map_err(io)?;
        let current = Active::read(&self.orbit)?.generation();
        if current != self.source {
            return Err(Error::SourceChanged);
        }
        let pointer = Pointer::new(self.generation, self.source, self.evidence);
        let temporary = self.orbit.join(format!("{ACTIVE_FILE}.next"));
        write_sync(&temporary, &pointer.encode()?)?;
        atomic_replace(&temporary, &self.orbit.join(ACTIVE_FILE)).map_err(io)?;
        sync_dir(&self.orbit).map_err(io)?;
        Ok(Activation {
            generation: self.generation,
            evidence: self.evidence,
        })
    }
}

/// The durable result of selecting a verified generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Activation {
    generation: Generation,
    evidence: Evidence,
}

impl Activation {
    pub fn generation(self) -> Generation {
        self.generation
    }

    pub fn evidence(self) -> Evidence {
        self.evidence
    }
}

fn write_sync(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(io)?;
    file.write_all(bytes).map_err(io)?;
    file.sync_all().map_err(io)
}

fn sync_tree(root: &Path) -> Result<(), Error> {
    for entry in std::fs::read_dir(root).map_err(io)? {
        let entry = entry.map_err(io)?;
        let kind = entry.file_type().map_err(io)?;
        if kind.is_dir() {
            sync_tree(&entry.path())?;
        } else if kind.is_file() {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(entry.path())
                .and_then(|file| file.sync_all())
                .map_err(io)?;
        }
    }
    sync_dir(root).map_err(io)
}

fn atomic_replace(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    let mut last = None;
    for attempt in 0..5 {
        match std::fs::rename(temporary, destination) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last = Some(error);
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(10 << attempt));
                }
            }
        }
    }
    last.map_or_else(|| Ok(()), Err)
}

#[cfg(unix)]
fn sync_dir(directory: &Path) -> std::io::Result<()> {
    File::open(directory).and_then(|file| file.sync_all())
}

#[cfg(windows)]
fn sync_dir(directory: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(directory)
        .or_else(|_| {
            OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(directory)
        });
    match handle {
        Err(_) => Ok(()),
        Ok(file) => file.sync_all(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn orbit() -> std::path::PathBuf {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("lait-generation-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("authority")).unwrap();
        path
    }

    #[test]
    fn an_orbit_without_a_pointer_is_the_implicit_generation() {
        let root = orbit();
        let active = Active::read(&root).unwrap();
        assert_eq!(active.generation(), None);
        assert_eq!(active.path(Component::Replica), root);
        assert_eq!(active.path(Component::Mechanics), root.join("authority"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn build_verify_activate_switches_both_components_together() {
        let root = orbit();
        let generation = Generation::derive(None, b"journal-v2");
        let build = Build::begin(&root, generation).unwrap();
        std::fs::write(build.path(Component::Replica).join("body"), b"body").unwrap();
        std::fs::write(build.path(Component::Mechanics).join("effects"), b"effects").unwrap();

        let verification = build.verify(Evidence::from_digest([7; 32])).unwrap();
        let activation = verification.activate().unwrap();
        assert_eq!(activation.generation(), generation);

        let active = Active::read(&root).unwrap();
        assert_eq!(active.generation(), Some(generation));
        assert_eq!(
            std::fs::read(active.path(Component::Replica).join("body")).unwrap(),
            b"body"
        );
        assert_eq!(
            std::fs::read(active.path(Component::Mechanics).join("effects")).unwrap(),
            b"effects"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn activation_is_a_compare_and_swap() {
        let root = orbit();
        let a = Build::begin(&root, Generation::derive(None, b"a"))
            .unwrap()
            .verify(Evidence::from_digest([1; 32]))
            .unwrap();
        let b = Build::begin(&root, Generation::derive(None, b"b"))
            .unwrap()
            .verify(Evidence::from_digest([2; 32]))
            .unwrap();

        a.activate().unwrap();
        assert_eq!(b.activate().unwrap_err(), Error::SourceChanged);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_or_noncanonical_pointer_is_refused() {
        let root = orbit();
        std::fs::write(root.join(ACTIVE_FILE), b"not a generation").unwrap();
        assert_eq!(Active::read(&root).unwrap_err(), Error::CorruptPointer);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_interrupted_unverified_build_can_restart() {
        let root = orbit();
        let generation = Generation::derive(None, b"retry");
        let build = std::mem::ManuallyDrop::new(Build::begin(&root, generation).unwrap());
        std::fs::write(build.path(Component::Replica).join("partial"), b"partial").unwrap();

        let restarted = Build::restart(&root, generation).unwrap();
        assert!(!restarted.path(Component::Replica).join("partial").exists());
        drop(restarted);
        let _ = std::fs::remove_dir_all(root);
    }
}
