//! Crash-safe persistence of workbench state.
//!
//! Rules this module enforces:
//! - Readers never see a partly written file: a save writes a uniquely named
//!   temporary file in the same directory, fsyncs it and renames it over the
//!   target. A failure before the rename leaves the previous file intact.
//!   After the rename the directory is fsynced (on Unix) to make the
//!   replacement durable; if only that step fails, the new state is already
//!   in place and the error says so.
//! - Concurrent saves to one path never corrupt it: each uses its own
//!   temporary file and the last rename wins. Writers are not otherwise
//!   coordinated; one application instance per state directory is assumed.
//! - A file that cannot be understood is never overwritten or deleted. It is
//!   moved aside under a new name and reported, and the session starts fresh.
//! - A file written by a newer schema is left exactly as it is, and the
//!   session does not save over it.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Version of [`PersistedState`]'s layout. Bump on incompatible change and
/// add a migration in [`StateStore::load`].
pub const STATE_SCHEMA_VERSION: u32 = 1;

/// An open document as saved: what opens it again, and its own state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PersistedDocument {
    pub kind: String,
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
}

/// Everything restored at the next start.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PersistedState {
    pub schema: u32,
    #[serde(default)]
    pub documents: Vec<PersistedDocument>,
    #[serde(default)]
    pub active_document: Option<usize>,
    /// Active view id per dock location (`left`, `bottom`).
    #[serde(default)]
    pub active_views: BTreeMap<String, String>,
}

/// What [`StateStore::load`] found.
#[derive(Debug, PartialEq)]
pub enum LoadOutcome {
    /// No state file exists yet.
    Fresh,
    Loaded(PersistedState),
    /// The file was unreadable as state; it now lives at `moved_to`.
    Quarantined {
        moved_to: PathBuf,
        reason: String,
    },
    /// The file belongs to a newer version and was left untouched. Saving is
    /// disabled for this store.
    NewerSchema {
        found: u32,
    },
}

/// Reads and atomically replaces one state file.
pub struct StateStore {
    path: PathBuf,
    read_only: bool,
}

impl StateStore {
    /// A store backed by `path`. Nothing is read or created yet.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            read_only: false,
        }
    }

    /// The file this store reads and replaces.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether saving is disabled because the file is from a newer version.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Reads the state file. A file that cannot be understood is moved aside
    /// and reported; a file from a newer schema makes this store read-only.
    /// Errors are I/O failures other than a missing file.
    pub fn load(&mut self) -> io::Result<LoadOutcome> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(LoadOutcome::Fresh);
            }
            Err(error) => return Err(error),
        };
        // Read the schema number first: a newer file may not deserialize as
        // today's layout, and that must not be mistaken for corruption.
        let schema = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| value.get("schema").and_then(Value::as_u64));
        match schema {
            Some(found) if found > u64::from(STATE_SCHEMA_VERSION) => {
                self.read_only = true;
                Ok(LoadOutcome::NewerSchema {
                    found: u32::try_from(found).unwrap_or(u32::MAX),
                })
            }
            Some(found) if found == u64::from(STATE_SCHEMA_VERSION) => {
                match serde_json::from_slice::<PersistedState>(&bytes) {
                    Ok(state) => Ok(LoadOutcome::Loaded(state)),
                    Err(error) => self.quarantine(error.to_string()),
                }
            }
            Some(found) => self.quarantine(format!("unsupported schema version {found}")),
            None => self.quarantine("not a state file: no numeric `schema` field".to_owned()),
        }
    }

    fn quarantine(&self, reason: String) -> io::Result<LoadOutcome> {
        let moved_to = (1u32..)
            .map(|n| {
                let mut name = self.path.as_os_str().to_owned();
                name.push(format!(".corrupt-{n}"));
                PathBuf::from(name)
            })
            .find(|candidate| !candidate.exists())
            .expect("an unused quarantine name exists");
        fs::rename(&self.path, &moved_to)?;
        Ok(LoadOutcome::Quarantined { moved_to, reason })
    }

    /// Atomically replaces the stored state. Returns `Ok(false)` without
    /// writing when the store is read-only.
    ///
    /// On `Err` the outcome depends on where it failed, and the error message
    /// says which: a failure before the rename leaves the previous file
    /// exactly as it was; a failure syncing the directory after the rename
    /// means the new state is in place but may not survive a power loss.
    pub fn save(&self, state: &PersistedState) -> io::Result<bool> {
        if self.read_only {
            return Ok(false);
        }
        let directory = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(directory)?;
        let mut bytes = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
        bytes.push(b'\n');

        // Unique per save, so two stores or threads writing the same path
        // never share a temporary file; `create_new` turns any remaining
        // collision into an error instead of an overwrite.
        static SAVE_COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut temporary = self.path.as_os_str().to_owned();
        temporary.push(format!(
            ".tmp-{}-{}",
            std::process::id(),
            SAVE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary = PathBuf::from(temporary);

        let replaced = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &self.path)
        })();
        if let Err(error) = replaced {
            // The previous state file was not touched.
            let _ = fs::remove_file(&temporary);
            return Err(io::Error::new(
                error.kind(),
                format!("state was not saved, the previous file is unchanged: {error}"),
            ));
        }
        sync_directory(directory).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("state was saved but the directory could not be synced: {error}"),
            )
        })?;
        Ok(true)
    }
}

/// Makes the rename durable. Directories cannot be opened for syncing on
/// Windows, where the rename is journalled by the file system instead.
fn sync_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(directory)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PersistedState {
        PersistedState {
            schema: STATE_SCHEMA_VERSION,
            documents: vec![PersistedDocument {
                kind: "logs.source".into(),
                resource: "sample.auth".into(),
                state: Some(serde_json::json!({ "filter": "level:error" })),
            }],
            active_document: Some(0),
            active_views: BTreeMap::from([("bottom".into(), "output".into())]),
        }
    }

    fn entries(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn missing_file_is_a_fresh_start() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = StateStore::new(directory.path().join("state.json"));
        assert_eq!(store.load().unwrap(), LoadOutcome::Fresh);
    }

    #[test]
    fn save_then_load_round_trips_and_leaves_no_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/dir/state.json");
        let mut store = StateStore::new(&path);
        assert!(store.save(&sample()).unwrap());
        assert_eq!(store.load().unwrap(), LoadOutcome::Loaded(sample()));
        assert_eq!(entries(path.parent().unwrap()), ["state.json"]);

        let mut changed = sample();
        changed.active_document = None;
        assert!(store.save(&changed).unwrap());
        assert_eq!(store.load().unwrap(), LoadOutcome::Loaded(changed));
    }

    #[test]
    fn corrupt_file_is_moved_aside_with_its_bytes_intact() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        for (n, garbage) in [
            &b"{ truncated"[..],
            b"[]",
            b"{\"schema\": 1, \"documents\": 7}",
        ]
        .into_iter()
        .enumerate()
        {
            fs::write(&path, garbage).unwrap();
            let mut store = StateStore::new(&path);
            let LoadOutcome::Quarantined { moved_to, reason } = store.load().unwrap() else {
                panic!("expected quarantine for {garbage:?}");
            };
            assert_eq!(
                moved_to,
                directory
                    .path()
                    .join(format!("state.json.corrupt-{}", n + 1))
            );
            assert!(!reason.is_empty());
            assert_eq!(fs::read(&moved_to).unwrap(), garbage);
            assert!(!path.exists());
            // The session can save normally afterwards.
            assert!(store.save(&sample()).unwrap());
            assert_eq!(store.load().unwrap(), LoadOutcome::Loaded(sample()));
        }
    }

    #[test]
    fn older_unknown_schema_is_quarantined_not_guessed_at() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        fs::write(&path, b"{\"schema\": 0}").unwrap();
        let mut store = StateStore::new(&path);
        assert!(matches!(
            store.load().unwrap(),
            LoadOutcome::Quarantined { reason, .. } if reason.contains("version 0")
        ));
    }

    #[test]
    fn newer_schema_is_left_untouched_and_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let newer = br#"{"schema": 2, "documents": "a layout this version cannot read"}"#;
        fs::write(&path, newer).unwrap();

        let mut store = StateStore::new(&path);
        assert_eq!(store.load().unwrap(), LoadOutcome::NewerSchema { found: 2 });
        assert!(store.is_read_only());
        assert!(!store.save(&sample()).unwrap(), "save must be skipped");
        assert_eq!(fs::read(&path).unwrap(), newer);
        assert_eq!(entries(directory.path()), ["state.json"]);
    }

    #[cfg(unix)]
    #[test]
    fn failed_save_keeps_the_previous_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let mut store = StateStore::new(&path);
        store.save(&sample()).unwrap();

        // A directory that cannot be written to: the temporary file cannot be
        // created, so the save fails before the rename.
        use std::os::unix::fs::PermissionsExt as _;
        let writable = fs::metadata(directory.path()).unwrap().permissions();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let mut changed = sample();
        changed.documents.clear();
        let error = store.save(&changed).unwrap_err();
        fs::set_permissions(directory.path(), writable).unwrap();

        assert!(
            error.to_string().contains("previous file is unchanged"),
            "{error}"
        );
        assert_eq!(store.load().unwrap(), LoadOutcome::Loaded(sample()));
        assert_eq!(entries(directory.path()), ["state.json"]);
    }

    #[test]
    fn concurrent_saves_never_leave_a_torn_or_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let variants: Vec<PersistedState> = (0..8)
            .map(|n| {
                let mut state = sample();
                state.documents[0].resource = format!("resource-{n}");
                state
            })
            .collect();
        std::thread::scope(|scope| {
            for state in &variants {
                let path = path.clone();
                scope.spawn(move || {
                    let store = StateStore::new(path);
                    for _ in 0..25 {
                        store.save(state).unwrap();
                    }
                });
            }
        });
        let LoadOutcome::Loaded(loaded) = StateStore::new(&path).load().unwrap() else {
            panic!("state file is not readable after concurrent saves");
        };
        assert!(variants.contains(&loaded), "file holds one complete save");
        assert_eq!(entries(directory.path()), ["state.json"]);
    }
}
