//! The browser medium: OPFS sync access handles in a dedicated worker.
//!
//! OPFS is the one fast, synchronous storage a browser offers, and it comes
//! with two hard constraints the design here exists to absorb. **Acquiring**
//! a handle is asynchronous — and in a worker without shared-memory atomics
//! nothing can block on a promise — while the [`Medium`] seam is synchronous
//! and may need a brand-new slot in the middle of a compaction. And
//! **renaming** is not a primitive worth trusting. So this medium is a
//! **pool**, the shape SQLite's OPFS VFS proved: a directory of physical
//! files (`pool-<n>`) whose sync handles are all acquired up front, each
//! carrying a 64-byte header ([`crate::pool_header`]) that names the logical
//! slot it currently hosts — or names nothing, making it a spare. Opening a
//! new slot takes a spare synchronously; removal recycles the file back into
//! the pool; a background task replenishes spares between the engine's
//! turns. Every trait offset is biased past the header, invisibly.
//!
//! The recycle protocols, validated adversarially (flush durability here is
//! implementation-defined, so nothing may depend on a prior flush):
//!
//! - **assign**: truncate to the header, write the name, flush. The truncate
//!   is unconditional — a "spare" may carry resurrected bytes from a life a
//!   crash brought back.
//! - **recycle**: truncate to the header, flush, clear the name, flush. In
//!   that order: the state a crash can leave is *named-but-empty*, which the
//!   pack rules unusable and removes again — convergent. The other order
//!   could leave a spare with stale data, the one state that feeds the
//!   resurrection hazard.
//! - What closes resurrection structurally is above this module: the pack's
//!   own slot header records its logical name, so a whole old slot coming
//!   back under a recycled file is rejected as history, not elected.
//!
//! The sync handle's exclusive lock is the single-writer guard — a second
//! tab's construction fails acquiring handles and reports "another tab owns
//! this store" distinguishably. (A Web Locks advisory layer can join it when
//! multi-tab arbitration UX matters; the enforcement does not need it.)
//! `persist()` cannot be *requested* from a worker — only queried — so this
//! medium reports [`OpfsMedium::persisted`] and the embedding page owns the
//! request.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use send_wrapper::SendWrapper;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions,
    FileSystemGetFileOptions, FileSystemReadWriteOptions, FileSystemSyncAccessHandle,
    WorkerGlobalScope,
};

use crate::medium::{Medium, ReadAt, SlotWriter};
use crate::pool_header::{decode, encode, POOL_HEADER_LEN, POOL_NAME_CAPACITY};

/// Spares kept ready so compaction's successor never waits.
const SPARE_TARGET: usize = 4;
/// Ceiling on physical files, spares included: recycling returns files to
/// the pool, so without a cap replenish-after-take would ratchet forever.
const CAPACITY: usize = 16;
/// Reload races hold the old worker's locks briefly; retry before refusing.
const ACQUIRE_ATTEMPTS: u32 = 20;
const ACQUIRE_BACKOFF_MS: i32 = 50;

const BIAS: u64 = POOL_HEADER_LEN as u64;

fn js_io(context: &str, error: &JsValue) -> std::io::Error {
    let name = error
        .dyn_ref::<js_sys::Object>()
        .and_then(|o| js_sys::Reflect::get(o, &JsValue::from_str("name")).ok())
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    let kind = match name.as_str() {
        "NoModificationAllowedError" => std::io::ErrorKind::WouldBlock,
        "QuotaExceededError" => std::io::ErrorKind::QuotaExceeded,
        "NotFoundError" => std::io::ErrorKind::NotFound,
        "TypeMismatchError" => std::io::ErrorKind::InvalidInput,
        "SecurityError" | "NotAllowedError" => std::io::ErrorKind::PermissionDenied,
        "AbortError" => std::io::ErrorKind::Interrupted,
        _ => std::io::ErrorKind::Other,
    };
    std::io::Error::new(kind, format!("{context}: {name}"))
}

fn poisoned() -> std::io::Error {
    std::io::Error::other("opfs pool state poisoned")
}

/// One acquired sync handle. Closed on drop, which is what returns the
/// file's lock to the origin.
struct Handle {
    inner: FileSystemSyncAccessHandle,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.inner.close();
    }
}

impl Handle {
    fn size(&self) -> Result<u64, std::io::Error> {
        let size = self.inner.get_size().map_err(|e| js_io("get_size", &e))?;
        // f64 is exact to 2^53; a slot cannot approach it.
        Ok(size as u64)
    }

    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result<(), std::io::Error> {
        let options = FileSystemReadWriteOptions::new();
        options.set_at(at as f64);
        let read = self
            .inner
            .read_with_u8_array_and_options(buf, &options)
            .map_err(|e| js_io("read", &e))?;
        if read as u64 != buf.len() as u64 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        Ok(())
    }

    fn write_all_at(&self, at: u64, bytes: &[u8]) -> Result<(), std::io::Error> {
        let options = FileSystemReadWriteOptions::new();
        options.set_at(at as f64);
        let written = self
            .inner
            .write_with_u8_array_and_options(bytes, &options)
            .map_err(|e| js_io("write", &e))?;
        if written as u64 != bytes.len() as u64 {
            return Err(std::io::Error::from(std::io::ErrorKind::WriteZero));
        }
        Ok(())
    }

    fn truncate(&self, len: u64) -> Result<(), std::io::Error> {
        self.inner
            .truncate_with_f64(len as f64)
            .map_err(|e| js_io("truncate", &e))
    }

    fn flush(&self) -> Result<(), std::io::Error> {
        self.inner.flush().map_err(|e| js_io("flush", &e))
    }
}

struct PoolFile {
    handle: Rc<Handle>,
}

struct PoolState {
    assigned: BTreeMap<String, PoolFile>,
    spares: Vec<PoolFile>,
    next_physical: u64,
    replenishing: bool,
}

struct Shared {
    dir: FileSystemDirectoryHandle,
    state: RefCell<PoolState>,
    persisted: bool,
}

/// The OPFS-backed [`Medium`]. Constructed asynchronously — handle
/// acquisition cannot happen any other way — then synchronous forever.
pub struct OpfsMedium {
    shared: SendWrapper<Rc<Shared>>,
}

impl std::fmt::Debug for OpfsMedium {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.shared.state.borrow();
        f.debug_struct("OpfsMedium")
            .field("assigned", &state.assigned.len())
            .field("spares", &state.spares.len())
            .field("persisted", &self.shared.persisted)
            .finish_non_exhaustive()
    }
}

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Ok(scope) = js_sys::global().dyn_into::<WorkerGlobalScope>() {
            let _ = scope.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
}

async fn acquire(file: &FileSystemFileHandle) -> Result<Handle, std::io::Error> {
    let mut last = std::io::Error::from(std::io::ErrorKind::WouldBlock);
    for attempt in 0..ACQUIRE_ATTEMPTS {
        match JsFuture::from(file.create_sync_access_handle()).await {
            Ok(value) => {
                let inner: FileSystemSyncAccessHandle =
                    value.dyn_into().map_err(|e| js_io("handle cast", &e))?;
                return Ok(Handle { inner });
            }
            Err(error) => {
                let mapped = js_io("create_sync_access_handle", &error);
                if mapped.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(mapped);
                }
                last = mapped;
                if attempt + 1 < ACQUIRE_ATTEMPTS {
                    sleep_ms(ACQUIRE_BACKOFF_MS).await;
                }
            }
        }
    }
    // Every attempt found the lock held: another tab owns this store. The
    // distinguishable kind is the message the surface above should show.
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!("another tab holds this store: {last}"),
    ))
}

/// Re-establish a file as a spare: truncate away any life it carried, then
/// say so. The truncate never trusts prior flushes.
fn spare_out(handle: &Handle) -> Result<(), std::io::Error> {
    let spare = encode(None).ok_or_else(poisoned)?;
    handle.write_all_at(0, &spare)?;
    handle.truncate(BIAS)?;
    handle.flush()?;
    Ok(())
}

async fn create_physical(
    dir: &FileSystemDirectoryHandle,
    ordinal: u64,
) -> Result<PoolFile, std::io::Error> {
    let name = format!("pool-{ordinal}");
    let options = FileSystemGetFileOptions::new();
    options.set_create(true);
    let file: FileSystemFileHandle =
        JsFuture::from(dir.get_file_handle_with_options(&name, &options))
            .await
            .map_err(|e| js_io("get_file_handle", &e))?
            .dyn_into()
            .map_err(|e| js_io("file cast", &e))?;
    let handle = acquire(&file).await?;
    spare_out(&handle)?;
    Ok(PoolFile {
        handle: Rc::new(handle),
    })
}

impl OpfsMedium {
    /// Open (creating if absent) the store directory under the origin's
    /// private file system and acquire the whole pool. Async by necessity;
    /// everything after construction is synchronous.
    pub async fn open(store_dir: &str) -> Result<Self, std::io::Error> {
        let scope: WorkerGlobalScope = js_sys::global()
            .dyn_into()
            .map_err(|e| js_io("worker scope", &e))?;
        let storage = scope.navigator().storage();
        let persisted = match storage.persisted() {
            Ok(promise) => JsFuture::from(promise)
                .await
                .ok()
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            Err(_) => false,
        };
        let root: FileSystemDirectoryHandle = JsFuture::from(storage.get_directory())
            .await
            .map_err(|e| js_io("opfs root", &e))?
            .dyn_into()
            .map_err(|e| js_io("root cast", &e))?;
        let options = FileSystemGetDirectoryOptions::new();
        options.set_create(true);
        let dir: FileSystemDirectoryHandle =
            JsFuture::from(root.get_directory_handle_with_options(store_dir, &options))
                .await
                .map_err(|e| js_io("store dir", &e))?
                .dyn_into()
                .map_err(|e| js_io("dir cast", &e))?;

        let mut assigned = BTreeMap::new();
        let mut spares = Vec::new();
        let mut next_physical: u64 = 0;
        let names = existing_pool_files(&dir).await?;
        let mut acquired: Vec<(u64, Handle)> = Vec::new();
        for (ordinal, file_name) in &names {
            let file: FileSystemFileHandle = JsFuture::from(dir.get_file_handle(file_name))
                .await
                .map_err(|e| js_io("get_file_handle", &e))?
                .dyn_into()
                .map_err(|e| js_io("file cast", &e))?;
            match acquire(&file).await {
                Ok(handle) => acquired.push((*ordinal, handle)),
                Err(error) => {
                    // Two half-owners deadlock each other on a reload race:
                    // release everything this construction took before
                    // reporting whose store this is.
                    drop(acquired);
                    return Err(error);
                }
            }
        }
        for (ordinal, handle) in acquired {
            next_physical = next_physical.max(ordinal.saturating_add(1));
            let mut header = [0u8; POOL_HEADER_LEN];
            let readable = handle.size()? >= BIAS && handle.read_at(0, &mut header).is_ok();
            match readable.then(|| decode(&header)).and_then(Result::ok) {
                Some(Some(slot_name)) => {
                    assigned.insert(
                        slot_name,
                        PoolFile {
                            handle: Rc::new(handle),
                        },
                    );
                }
                _ => {
                    // Spare, torn, or foreign: one fate. Nothing here trusts
                    // a prior flush, so every spare is re-established.
                    spare_out(&handle)?;
                    spares.push(PoolFile {
                        handle: Rc::new(handle),
                    });
                }
            }
        }
        while spares.len() < SPARE_TARGET && assigned.len().saturating_add(spares.len()) < CAPACITY
        {
            spares.push(create_physical(&dir, next_physical).await?);
            next_physical = next_physical.saturating_add(1);
        }
        Ok(Self {
            shared: SendWrapper::new(Rc::new(Shared {
                dir,
                state: RefCell::new(PoolState {
                    assigned,
                    spares,
                    next_physical,
                    replenishing: false,
                }),
                persisted,
            })),
        })
    }

    /// Whether the origin's storage was exempt from eviction when this medium
    /// was constructed. A `false` here is a best-effort store: honest to
    /// report, not this layer's to fix — requesting persistence is a
    /// window-context act the embedding page owns.
    #[must_use]
    pub fn persisted(&self) -> bool {
        self.shared.persisted
    }

    fn schedule_replenish(shared: &Rc<Shared>) {
        {
            let Ok(mut state) = shared.state.try_borrow_mut() else {
                return;
            };
            if state.replenishing {
                return;
            }
            state.replenishing = true;
        }
        let shared = Rc::clone(shared);
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                let (ordinal, wanted) = {
                    let Ok(mut state) = shared.state.try_borrow_mut() else {
                        break;
                    };
                    let total = state.assigned.len().saturating_add(state.spares.len());
                    if state.spares.len() >= SPARE_TARGET || total >= CAPACITY {
                        state.replenishing = false;
                        return;
                    }
                    let ordinal = state.next_physical;
                    state.next_physical = ordinal.saturating_add(1);
                    (ordinal, true)
                };
                if !wanted {
                    break;
                }
                match create_physical(&shared.dir, ordinal).await {
                    Ok(file) => {
                        if let Ok(mut state) = shared.state.try_borrow_mut() {
                            state.spares.push(file);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "opfs spare replenish failed; will retry");
                        break;
                    }
                }
            }
            match shared.state.try_borrow_mut() {
                Ok(mut state) => state.replenishing = false,
                Err(_) => tracing::error!("opfs replenish flag stuck; pool will not refill"),
            }
        });
    }
}

/// Enumerate `pool-<n>` files already in the store directory, in ordinal
/// order — what a previous life left behind.
async fn existing_pool_files(
    dir: &FileSystemDirectoryHandle,
) -> Result<Vec<(u64, String)>, std::io::Error> {
    let mut found = Vec::new();
    let iter = dir.keys();
    loop {
        let step = JsFuture::from(iter.next().map_err(|e| js_io("dir iter", &e))?)
            .await
            .map_err(|e| js_io("dir iter", &e))?;
        let done = js_sys::Reflect::get(&step, &JsValue::from_str("done"))
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if done {
            break;
        }
        let Some(name) = js_sys::Reflect::get(&step, &JsValue::from_str("value"))
            .ok()
            .and_then(|v| v.as_string())
        else {
            continue;
        };
        if let Some(ordinal) = name
            .strip_prefix("pool-")
            .and_then(|digits| digits.parse::<u64>().ok())
        {
            found.push((ordinal, name));
        }
    }
    found.sort_unstable();
    Ok(found)
}

struct OpfsWriter {
    handle: SendWrapper<Rc<Handle>>,
    len: u64,
}

impl SlotWriter for OpfsWriter {
    fn len(&self) -> u64 {
        self.len
    }

    fn append(&mut self, bytes: &[u8]) -> Result<u64, std::io::Error> {
        let offset = self.len;
        self.handle
            .write_all_at(BIAS.saturating_add(offset), bytes)?;
        self.len = offset.saturating_add(bytes.len() as u64);
        Ok(offset)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        self.handle.flush()
    }

    fn truncate(&mut self, new_len: u64) -> Result<(), std::io::Error> {
        if new_len > self.len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "truncate never grows a slot",
            ));
        }
        self.handle.truncate(BIAS.saturating_add(new_len))?;
        self.len = new_len;
        Ok(())
    }
}

struct OpfsReadAt {
    handle: SendWrapper<Rc<Handle>>,
}

impl ReadAt for OpfsReadAt {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), std::io::Error> {
        self.handle.read_at(BIAS.saturating_add(offset), buf)
    }
}

impl Medium for OpfsMedium {
    fn open_slot(
        &self,
        name: &str,
    ) -> Result<(Box<dyn SlotWriter>, std::sync::Arc<dyn ReadAt>), std::io::Error> {
        if name.is_empty() || name.len() > POOL_NAME_CAPACITY {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "slot names fit the pool header",
            ));
        }
        let mut state = self.shared.state.try_borrow_mut().map_err(|_| poisoned())?;
        let file = if let Some(file) = state.assigned.get(name) {
            PoolFile {
                handle: Rc::clone(&file.handle),
            }
        } else {
            let Some(spare) = state.spares.pop() else {
                drop(state);
                // Synchronous code cannot acquire a handle; arm the async
                // replenish HERE — the exhausted call is exactly the one
                // that cannot rely on a previous take having armed it — so
                // the caller's yield-and-retry finds a spare waiting.
                Self::schedule_replenish(&self.shared);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "slot pool exhausted; replenishing",
                ));
            };
            let assign = (|| {
                let header = encode(Some(name)).ok_or_else(poisoned)?;
                spare.handle.truncate(BIAS)?;
                spare.handle.write_all_at(0, &header)?;
                spare.handle.flush()
            })();
            if let Err(error) = assign {
                // The file's state is uncertain but a spare's always is:
                // assignment never trusts one. Keep it pooled, not leaked.
                state.spares.push(spare);
                return Err(error);
            }
            state.assigned.insert(
                name.to_owned(),
                PoolFile {
                    handle: Rc::clone(&spare.handle),
                },
            );
            spare
        };
        drop(state);
        Self::schedule_replenish(&self.shared);
        let len = file.handle.size()?.saturating_sub(BIAS);
        Ok((
            Box::new(OpfsWriter {
                handle: SendWrapper::new(Rc::clone(&file.handle)),
                len,
            }),
            std::sync::Arc::new(OpfsReadAt {
                handle: SendWrapper::new(file.handle),
            }),
        ))
    }

    fn remove_slot(&self, name: &str) -> Result<(), std::io::Error> {
        let mut state = self.shared.state.try_borrow_mut().map_err(|_| poisoned())?;
        let Some(file) = state.assigned.remove(name) else {
            return Ok(());
        };
        // truncate, flush, clear, flush — in that order, so the one state a
        // crash can leave is named-but-empty, which the pack removes again.
        let recycle = (|| {
            file.handle.truncate(BIAS)?;
            file.handle.flush()?;
            let spare = encode(None).ok_or_else(poisoned)?;
            file.handle.write_all_at(0, &spare)?;
            file.handle.flush()
        })();
        // Pooled either way: a spare's state is never trusted, and a file
        // whose recycle failed midway must not leak out of both maps.
        state.spares.push(file);
        recycle
    }

    fn slot_names(&self) -> Result<Vec<String>, std::io::Error> {
        let state = self.shared.state.try_borrow().map_err(|_| poisoned())?;
        Ok(state.assigned.keys().cloned().collect())
    }
}
