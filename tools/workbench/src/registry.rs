use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

const REGISTRY_SCHEMA_VERSION: u32 = 1;
const REGISTRY_FILE: &str = "devices.json";
const LOCK_FILE: &str = ".workbench.lock";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RegisteredDevice {
    pub(crate) id: String,
    pub(crate) label: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegistryFile {
    schema_version: u32,
    devices: Vec<RegisteredDevice>,
}

/// A single-writer, atomically replaced registry for one workbench state root.
pub(crate) struct Registry {
    path: PathBuf,
    _lock: File,
}

impl Registry {
    pub(crate) fn open(state_root: &Path) -> Result<Self> {
        let lock_path = state_root.join(LOCK_FILE);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("open workbench lock {}", lock_path.display()))?;
        lock.try_lock_exclusive().map_err(|error| {
            anyhow!(
                "another workbench is already using {}: {error}",
                state_root.display()
            )
        })?;
        Ok(Self {
            path: state_root.join(REGISTRY_FILE),
            _lock: lock,
        })
    }

    pub(crate) fn load(&self) -> Result<Vec<RegisteredDevice>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read workbench registry {}", self.path.display()));
            }
        };
        let registry: RegistryFile = serde_json::from_str(&text)
            .with_context(|| format!("decode workbench registry {}", self.path.display()))?;
        if registry.schema_version != REGISTRY_SCHEMA_VERSION {
            return Err(anyhow!(
                "workbench registry {} has schema version {}, expected {}",
                self.path.display(),
                registry.schema_version,
                REGISTRY_SCHEMA_VERSION
            ));
        }
        Ok(registry.devices)
    }

    pub(crate) fn save(&self, devices: &[RegisteredDevice]) -> Result<()> {
        let registry = RegistryFile {
            schema_version: REGISTRY_SCHEMA_VERSION,
            devices: devices.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&registry).context("encode workbench registry")?;
        let temporary = self
            .path
            .with_extension(format!("json.tmp.{}", std::process::id()));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&temporary)
                .with_context(|| format!("create registry temporary {}", temporary.display()))?;
            file.write_all(&bytes)
                .with_context(|| format!("write registry temporary {}", temporary.display()))?;
            file.sync_all()
                .with_context(|| format!("sync registry temporary {}", temporary.display()))?;
            drop(file);
            std::fs::rename(&temporary, &self.path).with_context(|| {
                format!(
                    "replace workbench registry {} from {}",
                    self.path.display(),
                    temporary.display()
                )
            })?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrations_round_trip_and_replace_atomically() {
        let directory = tempfile::tempdir().expect("tempdir");
        let registry = Registry::open(directory.path()).expect("registry");
        registry
            .save(&[RegisteredDevice {
                id: "alice".into(),
                label: "Alice".into(),
            }])
            .expect("save");
        assert_eq!(
            registry.load().expect("load"),
            vec![RegisteredDevice {
                id: "alice".into(),
                label: "Alice".into(),
            }]
        );
        assert!(!directory
            .path()
            .join(format!("devices.json.tmp.{}", std::process::id()))
            .exists());
    }

    #[test]
    fn corrupt_or_future_registry_is_not_silently_erased() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            directory.path().join(REGISTRY_FILE),
            r#"{"schemaVersion":999,"devices":[]}"#,
        )
        .expect("write registry");
        let registry = Registry::open(directory.path()).expect("registry");
        assert!(registry.load().is_err());
    }

    #[test]
    fn one_state_root_has_one_writer() {
        let directory = tempfile::tempdir().expect("tempdir");
        let first = Registry::open(directory.path()).expect("first registry");
        assert!(Registry::open(directory.path()).is_err());
        drop(first);
        assert!(Registry::open(directory.path()).is_ok());
    }
}
