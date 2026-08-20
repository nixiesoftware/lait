//! Versioned binary envelope. Fail closed. Atomic replace. Never
//! `unwrap_or_default`.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fabric::BodyExport;
use serde::{Deserialize, Serialize};

use crate::codec::{decode, encode, SCHEMA_VERSION};
use crate::durable::atomic_replace;
use crate::engine::BookEngine;
use crate::types::Book;
use crate::{bounds, Error};

const MAGIC: &[u8; 8] = b"LAITABK1";
const ENVELOPE_FORMAT: u8 = 1;
const PREFIX: usize = 17; // magic + format + header_len + body_len
/// The header is a handful of postcard bytes. A file declaring more is not
/// ours, and the declared length is honoured only after this gate — the
/// history bound alone would let a hostile header buy a 4 GiB read.
const MAX_HEADER_BYTES: usize = 4096;

#[derive(Serialize, Deserialize)]
struct Header {
    schema: u8,
    lamport: u64,
}

/// The on-disk book: one file under the identity directory.
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// `identity_dir/addressbook.bin`.
    pub fn at(identity_dir: impl AsRef<Path>) -> Self {
        Self {
            path: identity_dir.as_ref().join("addressbook.bin"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open an existing envelope, or `None` if the file is absent.
    ///
    /// Absence is only "a new book" when no swap survivor exists. A crash
    /// between the replace's remove and rename leaves no main file while a
    /// fully-synced `.tmp` (and the previous `.bak`) still do — recovering
    /// from those is what keeps that window from silently emptying the book.
    pub fn open(&self) -> Result<Option<BookEngine>, Error> {
        if !self.path.exists() {
            return self.recover();
        }
        let bytes = read_limited(&self.path)?;
        let engine = decode_envelope(&bytes)?;
        Ok(Some(engine))
    }

    /// Recover from an interrupted replace: prefer the newer `.tmp` (it was
    /// synced before the swap began), fall back to `.bak`. A survivor that
    /// decodes is copied back into place; the survivor itself is kept. When
    /// both exist and both fail to decode, fail closed with the first error
    /// rather than answering an empty book.
    fn recover(&self) -> Result<Option<BookEngine>, Error> {
        let tmp = self.path.with_extension("bin.tmp");
        let bak = self.path.with_extension("bin.bak");
        let mut first_failure: Option<Error> = None;
        for candidate in [&tmp, &bak] {
            if !candidate.exists() {
                continue;
            }
            match read_limited(candidate).and_then(|bytes| decode_envelope(&bytes)) {
                Ok(engine) => {
                    fs::copy(candidate, &self.path)?;
                    return Ok(Some(engine));
                }
                Err(err) => {
                    if first_failure.is_none() {
                        first_failure = Some(err);
                    }
                }
            }
        }
        match first_failure {
            Some(err) => Err(err),
            None => Ok(None),
        }
    }

    /// Write the engine as a new atomic envelope. The previous file is kept
    /// as `addressbook.bin.bak` so a failed replace does not become an empty book.
    pub fn replace(&self, engine: &BookEngine) -> Result<(), Error> {
        let book = engine.book()?;
        let bytes = encode_envelope(engine, &book)?;
        if bytes.len() > bounds::MAX_ADDRESSBOOK_HISTORY_BYTES {
            return Err(Error::Bound("MAX_ADDRESSBOOK_HISTORY_BYTES"));
        }
        atomic_replace(&self.path, &bytes)
    }
}

fn encode_envelope(engine: &BookEngine, book: &Book) -> Result<Vec<u8>, Error> {
    let header = Header {
        schema: SCHEMA_VERSION,
        lamport: book.clock,
    };
    let header_bytes = encode(&header)?;
    let export = engine.export_body()?;
    let body = encode(&export)?;
    let header_len = u32::try_from(header_bytes.len()).map_err(|_| Error::Bound("header"))?;
    let body_len = u32::try_from(body.len()).map_err(|_| Error::Bound("body"))?;
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(ENVELOPE_FORMAT);
    out.extend_from_slice(&header_len.to_le_bytes());
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&body);
    let digest = blake3::hash(&out);
    out.extend_from_slice(digest.as_bytes());
    Ok(out)
}

fn decode_envelope(bytes: &[u8]) -> Result<BookEngine, Error> {
    if bytes.len() < PREFIX.saturating_add(32) {
        return Err(Error::Corrupt("truncated prefix"));
    }
    if bytes.get(..8) != Some(MAGIC.as_slice()) {
        return Err(Error::Corrupt("magic"));
    }
    let format = *bytes.get(8).ok_or(Error::Corrupt("format"))?;
    if format != ENVELOPE_FORMAT {
        return Err(Error::UnsupportedVersion(format));
    }
    let header_len = u32_at(bytes, 9)?;
    let body_len = u32_at(bytes, 13)?;
    let header_len_us = usize::try_from(header_len).map_err(|_| Error::Corrupt("header len"))?;
    let body_len_us = usize::try_from(body_len).map_err(|_| Error::Corrupt("body len"))?;
    if header_len_us > MAX_HEADER_BYTES {
        return Err(Error::Corrupt("header len"));
    }
    if body_len_us > bounds::MAX_ADDRESSBOOK_HISTORY_BYTES {
        return Err(Error::Bound("MAX_ADDRESSBOOK_HISTORY_BYTES"));
    }
    let header_start = PREFIX;
    let body_start = header_start.saturating_add(header_len_us);
    let mac_start = body_start.saturating_add(body_len_us);
    let mac_end = mac_start.saturating_add(32);
    if bytes.len() != mac_end {
        return Err(Error::Corrupt("trailing or truncated body"));
    }
    let declared = bytes.get(..mac_start).ok_or(Error::Corrupt("slice"))?;
    let mac = bytes.get(mac_start..mac_end).ok_or(Error::Corrupt("mac"))?;
    if blake3::hash(declared).as_bytes() != mac {
        return Err(Error::Corrupt("checksum"));
    }
    let header_bytes = bytes
        .get(header_start..body_start)
        .ok_or(Error::Corrupt("header"))?;
    let header: Header = decode(header_bytes)?;
    if header.schema != SCHEMA_VERSION {
        return Err(Error::UnsupportedVersion(header.schema));
    }
    let body = bytes
        .get(body_start..mac_start)
        .ok_or(Error::Corrupt("body"))?;
    let export: BodyExport = decode(body)?;
    BookEngine::import_body(&export)
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let slice = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or(Error::Corrupt("u32"))?;
    let arr: [u8; 4] = slice.try_into().map_err(|_| Error::Corrupt("u32"))?;
    Ok(u32::from_le_bytes(arr))
}

fn read_limited(path: &Path) -> Result<Vec<u8>, Error> {
    let mut file = File::open(path)?;
    let mut prefix = [0u8; PREFIX];
    if file.read_exact(&mut prefix).is_err() {
        return Err(Error::Corrupt("truncated prefix"));
    }
    if prefix.get(..8) != Some(MAGIC.as_slice()) {
        return Err(Error::Corrupt("magic"));
    }
    let header_len =
        usize::try_from(u32_at(&prefix, 9)?).map_err(|_| Error::Corrupt("header len"))?;
    let body_len = usize::try_from(u32_at(&prefix, 13)?).map_err(|_| Error::Corrupt("body len"))?;
    if header_len > MAX_HEADER_BYTES {
        return Err(Error::Corrupt("header len"));
    }
    if body_len > bounds::MAX_ADDRESSBOOK_HISTORY_BYTES {
        return Err(Error::Bound("MAX_ADDRESSBOOK_HISTORY_BYTES"));
    }
    let total = PREFIX
        .saturating_add(header_len)
        .saturating_add(body_len)
        .saturating_add(32);
    let declared = u64::try_from(total).map_err(|_| Error::Corrupt("length"))?;
    // The declared lengths must account for the whole file: bytes past them
    // would be silently discarded, and the checksum over the declared prefix
    // would still verify — an appended-to file must read as corrupt.
    let actual = file.metadata()?.len();
    if actual != declared {
        return Err(Error::Corrupt("trailing or truncated bytes"));
    }
    let rest =
        declared.saturating_sub(u64::try_from(PREFIX).map_err(|_| Error::Corrupt("length"))?);
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(&prefix);
    file.take(rest).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| Error::Corrupt("length"))? != declared {
        return Err(Error::Corrupt("trailing or truncated bytes"));
    }
    Ok(bytes)
}
