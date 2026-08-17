//! Device-local encrypted persistence for exact immutable Corpus segments.
//!
//! Images are acceleration material, never authority. Immutable payload
//! segments are content-addressed and shared between publications; a small
//! encrypted manifest is the publication commit point. The Corpus codec still
//! verifies the portable publication coordinate, source fingerprint, BodyIx
//! stamps, and its internal segment shape before Runtime may install it. Every
//! file streams through bounded AEAD chunks into a temporary, fsync, and one
//! atomic rename, so interruption never publishes a partial cache line.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use mechanics::authorization::AuthorizedBodyKey;
use replica::body::WorldId;

use crate::publication::PublicationId;

const CACHE_DIR: &str = "corpus-images";
const SEGMENTS_DIR: &str = "segments";
const MANIFESTS_DIR: &str = "manifests";
const KEY_FILE: &str = "corpus-images.key";
const TEMP_SUFFIX: &str = ".tmp";
const SEGMENT_SUFFIX: &str = ".segment";
const MANIFEST_SUFFIX: &str = ".manifest";
const MAGIC: &[u8] = b"lait/corpus-image-encrypted/1\n";
const MANIFEST_MAGIC: &[u8] = b"lait/corpus-manifest/1\n";
const KEY_BYTES: usize = 16 + 32;
const CHUNK_BYTES: usize = 1024 * 1024;
const MAX_SEALED_CHUNK: usize = CHUNK_BYTES + mechanics::authorization::BODY_ENVELOPE_OVERHEAD;
const MAX_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SEGMENTS: usize = 1_000_000;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CODEC_MANIFEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAGE_PLAINTEXT_BYTES: u64 = 4 * 1024 * 1024 * 1024 * 1024;
const DEFAULT_CACHE_QUOTA_BYTES: u64 = 16 * 1024 * 1024 * 1024;

#[derive(Debug)]
pub(crate) enum Failure {
    Io(std::io::Error),
    Key,
    Crypto,
    Corrupt,
    Capacity,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for Failure {}

impl From<std::io::Error> for Failure {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CorpusImageStore {
    segments: PathBuf,
    manifests: PathBuf,
    key: AuthorizedBodyKey,
    quota_bytes: u64,
    pins: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<SegmentDigest, usize>>>,
    image_gate: std::sync::Arc<std::sync::Mutex<()>>,
}

/// Durable cache identity. MaterializationId is deliberately absent: it is a
/// Station-activation-local coordinate and resets. A semantically identical
/// publication is reusable only when the exact readable source material has
/// the same canonical fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CorpusImageKey {
    pub publication: PublicationId,
    pub source_fingerprint: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SegmentDigest([u8; 32]);

impl SegmentDigest {
    pub(crate) fn derive(bytes: &[u8]) -> Self {
        Self(blake3::derive_key("lait.corpus-image.segment.v1", bytes))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CorpusImageSegment {
    pub digest: SegmentDigest,
    pub plaintext_bytes: u64,
}

/// The tiny atomic publication commit point. Segment payloads are immutable,
/// content-addressed files, so adjacent Manifest roots share every unchanged
/// Corpus segment and publishing one changed Body never rewrites a full image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CorpusImageManifest {
    pub codec: u32,
    /// Opaque, bounded canonical Corpus codec manifest. Runtime storage does
    /// not reinterpret logical leaf kinds/ordinals; the Corpus decoder does.
    pub metadata: std::sync::Arc<[u8]>,
    pub segments: Vec<CorpusImageSegment>,
}

impl CorpusImageStore {
    pub(crate) fn open(orbit_dir: &Path) -> Result<Self, Failure> {
        Self::open_with_quota(orbit_dir, DEFAULT_CACHE_QUOTA_BYTES)
    }

    fn open_with_quota(orbit_dir: &Path, quota_bytes: u64) -> Result<Self, Failure> {
        if quota_bytes == 0 {
            return Err(Failure::Capacity);
        }
        let dir = orbit_dir.join(CACHE_DIR);
        mechanics::secretfs::create_private_dir(&dir).map_err(|_| Failure::Key)?;
        let segments = dir.join(SEGMENTS_DIR);
        let manifests = dir.join(MANIFESTS_DIR);
        mechanics::secretfs::create_private_dir(&segments).map_err(|_| Failure::Key)?;
        mechanics::secretfs::create_private_dir(&manifests).map_err(|_| Failure::Key)?;
        let key_path = dir.join(KEY_FILE);
        let key = match mechanics::secretfs::read_private(&key_path) {
            Ok(Some(bytes)) => match decode_key(&bytes) {
                Ok(key) => key,
                Err(_) => rotate_key(&key_path, [&segments, &manifests])?,
            },
            Ok(None) => rotate_key(&key_path, [&segments, &manifests])?,
            Err(_) => rotate_key(&key_path, [&segments, &manifests])?,
        };
        reclaim_temporaries(&segments)?;
        reclaim_temporaries(&manifests)?;
        let store = Self {
            segments,
            manifests,
            key,
            quota_bytes,
            pins: std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
            image_gate: std::sync::Arc::new(std::sync::Mutex::new(())),
        };
        // Startup cache cleanup is best-effort acceleration maintenance. The
        // lifecycle caller may still disable this store on an opening error,
        // but no cache defect is allowed to redefine durable World truth.
        let _ = store.sweep(None);
        Ok(store)
    }

    /// Install a complete logical image under one writer gate. Segment files
    /// become visible before the tiny Manifest rename, but no competing image
    /// sweep can collect that uncommitted set. Abandonment before the Manifest
    /// leaves only cache orphans, collected by the next successful commit.
    pub(crate) fn persist_segments(
        &self,
        world: &WorldId,
        key: CorpusImageKey,
        codec: u32,
        metadata: std::sync::Arc<[u8]>,
        payloads: &[std::sync::Arc<[u8]>],
    ) -> Result<(CorpusImageManifest, u64), Failure> {
        let _gate = self
            .image_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut segments = Vec::with_capacity(payloads.len());
        let mut written_bytes = 0u64;
        for payload in payloads {
            let (segment, written) = self.install_segment(payload)?;
            if written {
                written_bytes = written_bytes.saturating_add(segment.plaintext_bytes);
            }
            segments.push(segment);
        }
        let manifest = CorpusImageManifest {
            codec,
            metadata,
            segments,
        };
        self.commit_manifest(world, key, &manifest)?;
        Ok((manifest, written_bytes))
    }

    /// Installs one immutable plaintext segment. The boolean is false when an
    /// identical segment was already present and no payload bytes were written.
    pub(crate) fn install_segment(
        &self,
        plaintext: &[u8],
    ) -> Result<(CorpusImageSegment, bool), Failure> {
        if u64::try_from(plaintext.len()).unwrap_or(u64::MAX) > MAX_SEGMENT_BYTES {
            return Err(Failure::Corrupt);
        }
        let segment = CorpusImageSegment {
            digest: SegmentDigest::derive(plaintext),
            plaintext_bytes: u64::try_from(plaintext.len()).map_err(|_| Failure::Corrupt)?,
        };
        let path = self.segment_path(segment.digest);
        if path.try_exists()? {
            return Ok((segment, false));
        }
        let mut writer = self.encrypted_writer(path, &self.segments)?;
        writer.write_all(plaintext)?;
        writer.commit()?;
        Ok((segment, true))
    }

    pub(crate) fn read_segment(
        &self,
        segment: CorpusImageSegment,
    ) -> Result<Option<Vec<u8>>, Failure> {
        let path = self.segment_path(segment.digest);
        if segment.plaintext_bytes > MAX_SEGMENT_BYTES {
            return Err(Failure::Corrupt);
        }
        let Some(mut reader) = self.encrypted_reader(&path)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        if reader
            .by_ref()
            .take(MAX_SEGMENT_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .is_err()
            || u64::try_from(bytes.len()).ok() != Some(segment.plaintext_bytes)
            || SegmentDigest::derive(&bytes) != segment.digest
        {
            let _ = std::fs::remove_file(path);
            return Err(Failure::Corrupt);
        }
        Ok(Some(bytes))
    }

    pub(crate) fn commit_manifest(
        &self,
        world: &WorldId,
        key: CorpusImageKey,
        manifest: &CorpusImageManifest,
    ) -> Result<(), Failure> {
        self.validate_manifest(manifest)?;
        if manifest
            .segments
            .iter()
            .any(|segment| !self.segment_path(segment.digest).is_file())
        {
            return Err(Failure::Corrupt);
        }
        let bytes = encode_manifest(key, manifest)?;
        let path = self.manifest_path(world, key)?;
        let mut writer = self.encrypted_writer(path, &self.manifests)?;
        writer.write_all(&bytes)?;
        writer.commit()?;
        let protected = self.manifest_path(world, key)?;
        self.sweep(Some(&protected))
    }

    pub(crate) fn read_manifest(
        &self,
        world: &WorldId,
        key: CorpusImageKey,
    ) -> Result<Option<CorpusImageManifest>, Failure> {
        let path = self.manifest_path(world, key)?;
        let Some((stored_key, manifest)) = self.read_manifest_file(&path)? else {
            return Ok(None);
        };
        if stored_key != key {
            let _ = std::fs::remove_file(&path);
            return Err(Failure::Corrupt);
        }
        Ok(Some(manifest))
    }

    /// Pin every immutable segment of one exact image while a caller decodes
    /// it. Disk quota sweeping may evict old publication manifests, but it may
    /// not unlink a segment under an admitted reopen/build.
    pub(crate) fn lease_manifest(
        &self,
        world: &WorldId,
        key: CorpusImageKey,
    ) -> Result<Option<CorpusImageLease>, Failure> {
        let _gate = self
            .image_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(manifest) = self.read_manifest(world, key)? else {
            return Ok(None);
        };
        let mut pins = self.pins.lock().unwrap_or_else(|error| error.into_inner());
        for segment in &manifest.segments {
            let count = pins.entry(segment.digest).or_default();
            *count = count.saturating_add(1);
        }
        drop(pins);
        Ok(Some(CorpusImageLease {
            store: self.clone(),
            manifest,
        }))
    }

    fn read_manifest_file(
        &self,
        path: &Path,
    ) -> Result<Option<(CorpusImageKey, CorpusImageManifest)>, Failure> {
        let Some(mut reader) = self.encrypted_reader(path)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        let decoded = reader
            .by_ref()
            .take(MAX_MANIFEST_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| Failure::Corrupt)
            .and_then(|_| {
                if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
                    return Err(Failure::Corrupt);
                }
                decode_manifest(&bytes)
            });
        let (key, manifest) = match decoded {
            Ok(decoded) => decoded,
            Err(failure) => {
                let _ = std::fs::remove_file(path);
                return Err(failure);
            }
        };
        if self.validate_manifest(&manifest).is_err()
            || manifest
                .segments
                .iter()
                .any(|segment| !self.segment_path(segment.digest).is_file())
        {
            let _ = std::fs::remove_file(path);
            return Err(Failure::Corrupt);
        }
        Ok(Some((key, manifest)))
    }

    fn encrypted_writer(
        &self,
        final_path: PathBuf,
        directory: &Path,
    ) -> Result<CorpusImageWriter, Failure> {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|_| Failure::Key)?;
        let file_name = final_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(Failure::Corrupt)?;
        let temporary =
            final_path.with_file_name(format!("{file_name}.{}{}", hex(&nonce), TEMP_SUFFIX));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(MAGIC)?;
        Ok(CorpusImageWriter {
            key: self.key.clone(),
            file: Some(file),
            temporary,
            final_path,
            directory: directory.to_path_buf(),
            buffered: Vec::with_capacity(CHUNK_BYTES),
            plaintext_bytes: 0,
            digest: blake3::Hasher::new(),
            failed: false,
        })
    }

    fn encrypted_reader(&self, path: &Path) -> Result<Option<CorpusImageReader>, Failure> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Failure::Io(error)),
        };
        match CorpusImageReader::new(file, self.key.clone()) {
            Ok(reader) => Ok(Some(reader)),
            Err(failure) => {
                let _ = std::fs::remove_file(path);
                Err(failure)
            }
        }
    }

    fn manifest_path(&self, world: &WorldId, key: CorpusImageKey) -> Result<PathBuf, Failure> {
        let material = postcard::to_stdvec(&(world, key.publication, key.source_fingerprint))
            .map_err(|_| Failure::Corrupt)?;
        let digest = blake3::derive_key("lait.corpus-image.local-name.v1", &material);
        Ok(self
            .manifests
            .join(format!("{}{MANIFEST_SUFFIX}", hex(&digest))))
    }

    fn segment_path(&self, digest: SegmentDigest) -> PathBuf {
        self.segments
            .join(format!("{}{SEGMENT_SUFFIX}", hex(&digest.0)))
    }

    fn sweep(&self, protected: Option<&Path>) -> Result<(), Failure> {
        let pinned: std::collections::BTreeSet<_> = self
            .pins
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .copied()
            .collect();
        let mut referenced = pinned.clone();
        let mut used = 0u64;
        for digest in &pinned {
            used = used.saturating_add(
                std::fs::metadata(self.segment_path(*digest))
                    .map(|metadata| metadata.len())
                    .unwrap_or(0),
            );
        }
        if used > self.quota_bytes {
            return Err(Failure::Capacity);
        }

        let mut manifests = Vec::new();
        for entry in std::fs::read_dir(&self.manifests)? {
            let entry = entry?;
            if !entry.file_type()?.is_file()
                || !entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(MANIFEST_SUFFIX))
            {
                continue;
            }
            let path = entry.path();
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            match self.read_manifest_file(&path) {
                Ok(Some((_, manifest))) => manifests.push((path, modified, manifest)),
                Ok(None) | Err(Failure::Corrupt | Failure::Crypto) => {}
                Err(failure) => return Err(failure),
            }
        }
        manifests.sort_by(|left, right| {
            let left_protected = protected.is_some_and(|path| path == left.0);
            let right_protected = protected.is_some_and(|path| path == right.0);
            right_protected
                .cmp(&left_protected)
                .then_with(|| right.1.cmp(&left.1))
        });

        let mut rejected_protected = false;
        for (path, _, manifest) in manifests {
            let manifest_bytes = std::fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let additional = manifest
                .segments
                .iter()
                .filter(|segment| !referenced.contains(&segment.digest))
                .fold(manifest_bytes, |bytes, segment| {
                    bytes.saturating_add(
                        std::fs::metadata(self.segment_path(segment.digest))
                            .map(|metadata| metadata.len())
                            .unwrap_or(u64::MAX),
                    )
                });
            if used.saturating_add(additional) <= self.quota_bytes {
                used = used.saturating_add(additional);
                referenced.extend(manifest.segments.iter().map(|segment| segment.digest));
            } else {
                rejected_protected |= protected.is_some_and(|candidate| candidate == path);
                let _ = std::fs::remove_file(path);
            }
        }

        for entry in std::fs::read_dir(&self.segments)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(encoded) = name.strip_suffix(SEGMENT_SUFFIX) else {
                continue;
            };
            let Ok(raw) = data_encoding::HEXLOWER.decode(encoded.as_bytes()) else {
                let _ = std::fs::remove_file(entry.path());
                continue;
            };
            let Ok(digest) = <[u8; 32]>::try_from(raw.as_slice()) else {
                let _ = std::fs::remove_file(entry.path());
                continue;
            };
            if !referenced.contains(&SegmentDigest(digest)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        sync_dir(&self.manifests)?;
        sync_dir(&self.segments)?;
        if rejected_protected {
            Err(Failure::Capacity)
        } else {
            Ok(())
        }
    }

    fn validate_manifest(&self, manifest: &CorpusImageManifest) -> Result<(), Failure> {
        if manifest.segments.len() > MAX_SEGMENTS {
            return Err(Failure::Corrupt);
        }
        if manifest.metadata.len() > MAX_CODEC_MANIFEST_BYTES {
            return Err(Failure::Corrupt);
        }
        let mut total = 0u64;
        for segment in &manifest.segments {
            if segment.plaintext_bytes > MAX_SEGMENT_BYTES {
                return Err(Failure::Corrupt);
            }
            total = total
                .checked_add(segment.plaintext_bytes)
                .ok_or(Failure::Corrupt)?;
            if total > MAX_IMAGE_PLAINTEXT_BYTES {
                return Err(Failure::Corrupt);
            }
        }
        Ok(())
    }
}

fn encode_manifest(
    key: CorpusImageKey,
    manifest: &CorpusImageManifest,
) -> Result<Vec<u8>, Failure> {
    let count = u32::try_from(manifest.segments.len()).map_err(|_| Failure::Corrupt)?;
    let metadata_len = u32::try_from(manifest.metadata.len()).map_err(|_| Failure::Corrupt)?;
    let capacity = MANIFEST_MAGIC
        .len()
        .saturating_add(32 * 4)
        .saturating_add(12)
        .saturating_add(manifest.metadata.len())
        .saturating_add(manifest.segments.len().saturating_mul(40));
    if u64::try_from(capacity).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
        return Err(Failure::Corrupt);
    }
    let mut bytes = Vec::with_capacity(capacity);
    bytes.extend_from_slice(MANIFEST_MAGIC);
    bytes.extend_from_slice(&key.publication.manifest_root);
    bytes.extend_from_slice(&key.publication.implementation_digest);
    bytes.extend_from_slice(&key.publication.extractor_schema_digest.digest());
    bytes.extend_from_slice(&key.source_fingerprint);
    bytes.extend_from_slice(&manifest.codec.to_be_bytes());
    bytes.extend_from_slice(&metadata_len.to_be_bytes());
    bytes.extend_from_slice(&count.to_be_bytes());
    bytes.extend_from_slice(&manifest.metadata);
    for segment in &manifest.segments {
        bytes.extend_from_slice(&segment.digest.0);
        bytes.extend_from_slice(&segment.plaintext_bytes.to_be_bytes());
    }
    Ok(bytes)
}

fn decode_manifest(bytes: &[u8]) -> Result<(CorpusImageKey, CorpusImageManifest), Failure> {
    const FIXED_COORDINATES: usize = 32 * 4 + 4 + 4 + 4;
    let header = MANIFEST_MAGIC.len().saturating_add(FIXED_COORDINATES);
    if bytes.len() < header || &bytes[..MANIFEST_MAGIC.len()] != MANIFEST_MAGIC {
        return Err(Failure::Corrupt);
    }
    let mut at = MANIFEST_MAGIC.len();
    let manifest_root = take_array::<32>(bytes, &mut at)?;
    let implementation_digest = take_array::<32>(bytes, &mut at)?;
    let extractor_schema_digest = take_array::<32>(bytes, &mut at)?;
    let source_fingerprint = take_array::<32>(bytes, &mut at)?;
    let codec = u32::from_be_bytes(take_array::<4>(bytes, &mut at)?);
    let metadata_len = u32::from_be_bytes(take_array::<4>(bytes, &mut at)?) as usize;
    let count = u32::from_be_bytes(take_array::<4>(bytes, &mut at)?) as usize;
    if metadata_len > MAX_CODEC_MANIFEST_BYTES
        || count > MAX_SEGMENTS
        || bytes.len()
            != header
                .saturating_add(metadata_len)
                .saturating_add(count.saturating_mul(40))
    {
        return Err(Failure::Corrupt);
    }
    let metadata: std::sync::Arc<[u8]> = bytes
        .get(at..at.saturating_add(metadata_len))
        .ok_or(Failure::Corrupt)?
        .into();
    at = at.saturating_add(metadata_len);
    let mut segments = Vec::with_capacity(count);
    for _ in 0..count {
        segments.push(CorpusImageSegment {
            digest: SegmentDigest(take_array::<32>(bytes, &mut at)?),
            plaintext_bytes: u64::from_be_bytes(take_array::<8>(bytes, &mut at)?),
        });
    }
    Ok((
        CorpusImageKey {
            publication: PublicationId::new(
                manifest_root,
                implementation_digest,
                crate::publication::ExtractorSchemaDigest::from_digest(extractor_schema_digest),
            ),
            source_fingerprint,
        },
        CorpusImageManifest {
            codec,
            metadata,
            segments,
        },
    ))
}

fn take_array<const N: usize>(bytes: &[u8], at: &mut usize) -> Result<[u8; N], Failure> {
    let end = at.checked_add(N).ok_or(Failure::Corrupt)?;
    let value = bytes
        .get(*at..end)
        .ok_or(Failure::Corrupt)?
        .try_into()
        .map_err(|_| Failure::Corrupt)?;
    *at = end;
    Ok(value)
}

pub(crate) struct CorpusImageLease {
    store: CorpusImageStore,
    manifest: CorpusImageManifest,
}

impl CorpusImageLease {
    pub(crate) fn manifest(&self) -> &CorpusImageManifest {
        &self.manifest
    }

    pub(crate) fn plaintext_bytes(&self) -> u64 {
        self.manifest.segments.iter().fold(0u64, |bytes, segment| {
            bytes.saturating_add(segment.plaintext_bytes)
        })
    }

    pub(crate) fn read_segment(&self, segment: CorpusImageSegment) -> Result<Vec<u8>, Failure> {
        self.store.read_segment(segment)?.ok_or(Failure::Corrupt)
    }
}

impl Drop for CorpusImageLease {
    fn drop(&mut self) {
        let mut pins = self
            .store
            .pins
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for segment in &self.manifest.segments {
            let remove = pins.get_mut(&segment.digest).is_some_and(|count| {
                *count = count.saturating_sub(1);
                *count == 0
            });
            if remove {
                pins.remove(&segment.digest);
            }
        }
    }
}

pub(crate) struct CorpusImageWriter {
    key: AuthorizedBodyKey,
    file: Option<std::fs::File>,
    temporary: PathBuf,
    final_path: PathBuf,
    directory: PathBuf,
    buffered: Vec<u8>,
    plaintext_bytes: u64,
    digest: blake3::Hasher,
    failed: bool,
}

impl std::fmt::Debug for CorpusImageWriter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CorpusImageWriter")
            .field("temporary", &self.temporary)
            .field("final_path", &self.final_path)
            .field("plaintext_bytes", &self.plaintext_bytes)
            .finish_non_exhaustive()
    }
}

impl CorpusImageWriter {
    pub(crate) fn commit(mut self) -> Result<(), Failure> {
        if self.failed {
            return Err(Failure::Io(std::io::Error::other("image writer failed")));
        }
        self.write_buffered()?;
        let digest = *self.digest.finalize().as_bytes();
        let file = self.file.as_mut().ok_or(Failure::Corrupt)?;
        file.write_all(&[0])?;
        file.write_all(&self.plaintext_bytes.to_be_bytes())?;
        file.write_all(&digest)?;
        file.sync_all()?;
        self.file.take();
        mechanics::secretfs::persist_replace(&self.temporary, &self.final_path)?;
        sync_dir(&self.directory)?;
        Ok(())
    }

    fn write_buffered(&mut self) -> Result<(), Failure> {
        if self.buffered.is_empty() {
            return Ok(());
        }
        let plaintext = std::mem::take(&mut self.buffered);
        let sealed = mechanics::authorization::body_seal(&self.key, &plaintext)
            .map_err(|_| Failure::Crypto)?;
        let plain_len = u32::try_from(plaintext.len()).map_err(|_| Failure::Corrupt)?;
        let sealed_len = u32::try_from(sealed.len()).map_err(|_| Failure::Corrupt)?;
        let file = self.file.as_mut().ok_or(Failure::Corrupt)?;
        file.write_all(&[1])?;
        file.write_all(&plain_len.to_be_bytes())?;
        file.write_all(&sealed_len.to_be_bytes())?;
        file.write_all(&sealed)?;
        self.plaintext_bytes = self.plaintext_bytes.saturating_add(u64::from(plain_len));
        self.digest.update(&plaintext);
        self.buffered = Vec::with_capacity(CHUNK_BYTES);
        Ok(())
    }
}

impl Write for CorpusImageWriter {
    fn write(&mut self, mut bytes: &[u8]) -> std::io::Result<usize> {
        let original = bytes.len();
        while !bytes.is_empty() {
            let available = CHUNK_BYTES.saturating_sub(self.buffered.len());
            let take = available.min(bytes.len());
            self.buffered.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buffered.len() == CHUNK_BYTES {
                if let Err(error) = self.write_buffered() {
                    self.failed = true;
                    return Err(std::io::Error::other(error.to_string()));
                }
            }
        }
        Ok(original)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("closed image writer"))?
            .flush()
    }
}

pub(crate) struct CorpusImageReader {
    file: std::fs::File,
    key: AuthorizedBodyKey,
    current: Vec<u8>,
    at: usize,
    plaintext_bytes: u64,
    digest: blake3::Hasher,
    complete: bool,
}

impl std::fmt::Debug for CorpusImageReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CorpusImageReader")
            .field("plaintext_bytes", &self.plaintext_bytes)
            .field("complete", &self.complete)
            .finish_non_exhaustive()
    }
}

impl CorpusImageReader {
    fn new(mut file: std::fs::File, key: AuthorizedBodyKey) -> Result<Self, Failure> {
        let mut magic = vec![0u8; MAGIC.len()];
        file.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(Failure::Corrupt);
        }
        Ok(Self {
            file,
            key,
            current: Vec::new(),
            at: 0,
            plaintext_bytes: 0,
            digest: blake3::Hasher::new(),
            complete: false,
        })
    }

    fn next_chunk(&mut self) -> Result<bool, Failure> {
        let mut tag = [0u8; 1];
        self.file.read_exact(&mut tag)?;
        match tag[0] {
            0 => {
                let mut total = [0u8; 8];
                let mut expected = [0u8; 32];
                self.file.read_exact(&mut total)?;
                self.file.read_exact(&mut expected)?;
                if u64::from_be_bytes(total) != self.plaintext_bytes
                    || expected != *self.digest.finalize().as_bytes()
                {
                    return Err(Failure::Corrupt);
                }
                let mut trailing = [0u8; 1];
                if self.file.read(&mut trailing)? != 0 {
                    return Err(Failure::Corrupt);
                }
                self.complete = true;
                Ok(false)
            }
            1 => {
                let mut plain_len = [0u8; 4];
                let mut sealed_len = [0u8; 4];
                self.file.read_exact(&mut plain_len)?;
                self.file.read_exact(&mut sealed_len)?;
                let plain_len = u32::from_be_bytes(plain_len) as usize;
                let sealed_len = u32::from_be_bytes(sealed_len) as usize;
                if plain_len == 0
                    || plain_len > CHUNK_BYTES
                    || sealed_len > MAX_SEALED_CHUNK
                    || sealed_len < mechanics::authorization::BODY_ENVELOPE_OVERHEAD
                {
                    return Err(Failure::Corrupt);
                }
                let mut sealed = vec![0u8; sealed_len];
                self.file.read_exact(&mut sealed)?;
                let plaintext = mechanics::authorization::body_open(&self.key, &sealed)
                    .ok_or(Failure::Crypto)?;
                if plaintext.len() != plain_len {
                    return Err(Failure::Corrupt);
                }
                self.plaintext_bytes = self
                    .plaintext_bytes
                    .saturating_add(u64::try_from(plain_len).unwrap_or(u64::MAX));
                self.digest.update(&plaintext);
                self.current = plaintext;
                self.at = 0;
                Ok(true)
            }
            _ => Err(Failure::Corrupt),
        }
    }
}

impl Read for CorpusImageReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        while self.at == self.current.len() {
            self.current.clear();
            self.at = 0;
            if self.complete {
                return Ok(0);
            }
            match self.next_chunk() {
                Ok(true) => {}
                Ok(false) => return Ok(0),
                Err(error) => return Err(std::io::Error::other(error.to_string())),
            }
        }
        let available = &self.current[self.at..];
        let take = available.len().min(output.len());
        output[..take].copy_from_slice(&available[..take]);
        self.at = self.at.saturating_add(take);
        Ok(take)
    }
}

fn decode_key(bytes: &[u8]) -> Result<AuthorizedBodyKey, Failure> {
    if bytes.len() != KEY_BYTES {
        return Err(Failure::Key);
    }
    let epoch = bytes[..16].try_into().map_err(|_| Failure::Key)?;
    let key = bytes[16..].try_into().map_err(|_| Failure::Key)?;
    Ok(AuthorizedBodyKey::for_authorized_epoch(epoch, key))
}

fn create_key(path: &Path) -> Result<AuthorizedBodyKey, Failure> {
    let mut epoch = [0u8; 16];
    getrandom::fill(&mut epoch).map_err(|_| Failure::Key)?;
    let key = mechanics::authorization::random_key().map_err(|_| Failure::Key)?;
    let mut encoded = Vec::with_capacity(KEY_BYTES);
    encoded.extend_from_slice(&epoch);
    encoded.extend_from_slice(&key);
    mechanics::secretfs::write_private(
        path,
        &encoded,
        mechanics::secretfs::Create::New,
        mechanics::secretfs::Wrap::DeviceBound,
    )
    .map_err(|_| Failure::Key)?;
    Ok(AuthorizedBodyKey::for_authorized_epoch(epoch, key))
}

fn rotate_key<'a>(
    path: &Path,
    cache_dirs: impl IntoIterator<Item = &'a PathBuf>,
) -> Result<AuthorizedBodyKey, Failure> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(Failure::Key),
    }
    for dir in cache_dirs {
        for entry in std::fs::read_dir(dir).map_err(|_| Failure::Key)? {
            let entry = entry.map_err(|_| Failure::Key)?;
            if entry.file_type().map_err(|_| Failure::Key)?.is_file() {
                std::fs::remove_file(entry.path()).map_err(|_| Failure::Key)?;
            }
        }
    }
    create_key(path)
}

fn reclaim_temporaries(dir: &Path) -> Result<(), Failure> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(TEMP_SUFFIX))
        {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(Failure::Io(error)),
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), Failure> {
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn sync_dir(dir: &Path) -> Result<(), Failure> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)
        .or_else(|_| {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(dir)
        });
    match handle {
        Err(_) => Ok(()),
        Ok(directory) => directory.sync_all().map_err(Failure::Io),
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publication::{ExtractorSchemaDigest, PublicationId};

    fn temporary() -> PathBuf {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).expect("test entropy");
        let path = std::env::temp_dir().join(format!(
            "lait-corpus-image-{}-{}",
            std::process::id(),
            u64::from_le_bytes(random)
        ));
        std::fs::create_dir_all(&path).expect("temporary root");
        path
    }

    fn world() -> WorldId {
        WorldId::parse("com.example.corpus").expect("World")
    }

    fn image_key() -> CorpusImageKey {
        CorpusImageKey {
            publication: PublicationId::new(
                [1; 32],
                [2; 32],
                ExtractorSchemaDigest::from_digest([3; 32]),
            ),
            source_fingerprint: [4; 32],
        }
    }

    #[test]
    fn encrypted_segments_roundtrip_and_manifest_commits() {
        let root = temporary();
        let store = CorpusImageStore::open(&root).expect("store");
        let bytes = vec![0x5a; CHUNK_BYTES + 17];
        let (segment, written) = store.install_segment(&bytes).expect("segment");
        assert!(written);
        let manifest = CorpusImageManifest {
            codec: 1,
            metadata: std::sync::Arc::from([]),
            segments: vec![segment],
        };
        store
            .commit_manifest(&world(), image_key(), &manifest)
            .expect("manifest");

        let reopened = CorpusImageStore::open(&root).expect("reopen");
        assert_eq!(
            reopened
                .read_manifest(&world(), image_key())
                .expect("read manifest"),
            Some(manifest)
        );
        let decoded = reopened
            .read_segment(segment)
            .expect("read segment")
            .expect("segment exists");
        assert_eq!(decoded, bytes);
        let disk = std::fs::read(store.segment_path(segment.digest)).expect("disk");
        assert!(!disk.windows(32).any(|window| window == &bytes[..32]));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_manifest_never_replaces_complete_commit() {
        let root = temporary();
        let store = CorpusImageStore::open(&root).expect("store");
        let (segment, _) = store.install_segment(b"old-complete").expect("segment");
        let old = CorpusImageManifest {
            codec: 7,
            metadata: std::sync::Arc::from([]),
            segments: vec![segment],
        };
        store
            .commit_manifest(&world(), image_key(), &old)
            .expect("old commit");

        let manifest_path = store
            .manifest_path(&world(), image_key())
            .expect("manifest path");
        let mut interrupted = store
            .encrypted_writer(manifest_path, &store.manifests)
            .expect("new writer");
        interrupted
            .write_all(b"new-partial")
            .expect("partial bytes");
        drop(interrupted);
        let reopened = CorpusImageStore::open(&root).expect("reopen/reclaim");
        assert_eq!(
            reopened
                .read_manifest(&world(), image_key())
                .expect("old image intact"),
            Some(old)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn adjacent_publications_write_only_changed_segments() {
        let root = temporary();
        let store = CorpusImageStore::open(&root).expect("store");
        let unchanged = vec![0x11; CHUNK_BYTES];
        let changed = vec![0x22; 4096];
        let unchanged: std::sync::Arc<[u8]> = unchanged.into();
        let changed: std::sync::Arc<[u8]> = changed.into();
        let (first, first_written) = store
            .persist_segments(
                &world(),
                image_key(),
                1,
                std::sync::Arc::from([]),
                std::slice::from_ref(&unchanged),
            )
            .expect("first publication");
        assert_eq!(first_written, CHUNK_BYTES as u64);
        let shared = first.segments[0];
        let mut next_key = image_key();
        next_key.publication = PublicationId::new(
            [9; 32],
            [2; 32],
            ExtractorSchemaDigest::from_digest([3; 32]),
        );
        let (second, second_written) = store
            .persist_segments(
                &world(),
                next_key,
                1,
                std::sync::Arc::from([]),
                &[unchanged.clone(), changed.clone()],
            )
            .expect("second publication");
        assert_eq!(second_written, u64::try_from(changed.len()).unwrap());
        assert_eq!(second.segments[0], shared);

        assert_eq!(
            std::fs::read_dir(&store.segments)
                .expect("segments")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.ends_with(SEGMENT_SUFFIX))
                })
                .count(),
            2,
            "one changed segment adds one payload regardless of prior image size"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hostile_lengths_truncation_and_swapped_manifests_are_quarantined() {
        let root = temporary();
        let store = CorpusImageStore::open(&root).expect("store");
        let (segment, _) = store.install_segment(b"source").expect("segment");
        let manifest = CorpusImageManifest {
            codec: 1,
            metadata: std::sync::Arc::from([]),
            segments: vec![segment],
        };
        store
            .commit_manifest(&world(), image_key(), &manifest)
            .expect("manifest");

        std::fs::OpenOptions::new()
            .write(true)
            .open(store.segment_path(segment.digest))
            .expect("segment file")
            .set_len(u64::try_from(MAGIC.len() + 1).unwrap())
            .expect("truncate");
        assert!(store.read_segment(segment).is_err());
        assert!(!store.segment_path(segment.digest).exists());

        let (replacement, _) = store.install_segment(b"replacement").expect("replacement");
        let mut other_key = image_key();
        other_key.publication.manifest_root = [0x44; 32];
        store
            .commit_manifest(
                &world(),
                other_key,
                &CorpusImageManifest {
                    codec: 1,
                    metadata: std::sync::Arc::from([]),
                    segments: vec![replacement],
                },
            )
            .expect("other manifest");
        let original_path = store
            .manifest_path(&world(), image_key())
            .expect("original path");
        let other_path = store
            .manifest_path(&world(), other_key)
            .expect("other path");
        std::fs::copy(&other_path, &original_path).expect("swap manifest");
        assert!(store.read_manifest(&world(), image_key()).is_err());
        assert!(!original_path.exists(), "swapped entry is quarantined");

        let mut hostile = Vec::new();
        hostile.extend_from_slice(MANIFEST_MAGIC);
        hostile.extend_from_slice(&image_key().publication.manifest_root);
        hostile.extend_from_slice(&image_key().publication.implementation_digest);
        hostile.extend_from_slice(&image_key().publication.extractor_schema_digest.digest());
        hostile.extend_from_slice(&image_key().source_fingerprint);
        hostile.extend_from_slice(&1u32.to_be_bytes());
        hostile.extend_from_slice(&0u32.to_be_bytes());
        hostile.extend_from_slice(&u32::MAX.to_be_bytes());
        let mut writer = store
            .encrypted_writer(original_path.clone(), &store.manifests)
            .expect("hostile writer");
        writer.write_all(&hostile).expect("hostile payload");
        writer.commit().expect("hostile commit");
        assert!(store.read_manifest(&world(), image_key()).is_err());
        assert!(!original_path.exists(), "oversized entry is quarantined");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lost_device_key_rotates_and_discards_only_acceleration_material() {
        let root = temporary();
        let store = CorpusImageStore::open(&root).expect("store");
        let (segment, _) = store.install_segment(b"cached").expect("segment");
        store
            .commit_manifest(
                &world(),
                image_key(),
                &CorpusImageManifest {
                    codec: 1,
                    metadata: std::sync::Arc::from([]),
                    segments: vec![segment],
                },
            )
            .expect("manifest");
        std::fs::remove_file(root.join(CACHE_DIR).join(KEY_FILE)).expect("lose device key");

        let reopened = CorpusImageStore::open(&root).expect("cache key rotates");
        assert_eq!(
            reopened
                .read_manifest(&world(), image_key())
                .expect("old cache is a miss"),
            None
        );
        assert!(
            !reopened.segment_path(segment.digest).exists(),
            "unreadable acceleration payload is quarantined, not authoritative data"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn quota_never_evicts_segments_pinned_by_an_active_decode() {
        let root = temporary();
        let store = CorpusImageStore::open_with_quota(&root, 40 * 1024).expect("small store");
        let first_bytes = vec![0x31; 32 * 1024];
        let (first_segment, _) = store.install_segment(&first_bytes).expect("first segment");
        store
            .commit_manifest(
                &world(),
                image_key(),
                &CorpusImageManifest {
                    codec: 1,
                    metadata: std::sync::Arc::from([]),
                    segments: vec![first_segment],
                },
            )
            .expect("first manifest");
        let lease = store
            .lease_manifest(&world(), image_key())
            .expect("lease")
            .expect("image");

        let second_bytes = vec![0x32; 32 * 1024];
        let (second_segment, _) = store
            .install_segment(&second_bytes)
            .expect("second segment");
        let mut second_key = image_key();
        second_key.publication.manifest_root = [0x55; 32];
        assert!(matches!(
            store.commit_manifest(
                &world(),
                second_key,
                &CorpusImageManifest {
                    codec: 1,
                    metadata: std::sync::Arc::from([]),
                    segments: vec![second_segment],
                }
            ),
            Err(Failure::Capacity)
        ));
        assert_eq!(
            lease.read_segment(first_segment).expect("pinned bytes"),
            first_bytes
        );
        drop(lease);

        let (second_segment, _) = store
            .install_segment(&second_bytes)
            .expect("second segment retry");
        store
            .commit_manifest(
                &world(),
                second_key,
                &CorpusImageManifest {
                    codec: 1,
                    metadata: std::sync::Arc::from([]),
                    segments: vec![second_segment],
                },
            )
            .expect("newest image replaces unpinned old image");
        assert!(!store.segment_path(first_segment.digest).exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
