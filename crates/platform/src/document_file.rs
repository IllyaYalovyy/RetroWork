//! Bounded UTF-8 file snapshots and conflict-checked replacement.
//!
//! These operations perform filesystem I/O and belong on a worker thread.
//! Snapshots preserve BOMs and line endings without interpreting them.

use std::{
    fs::{self, File, Metadata},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

/// A file operation that could not complete. Failed saves retain the loaded baseline.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("not a regular file")]
    NotRegular,
    #[error("text exceeds the {0} byte limit")]
    TooLarge(u64),
    #[error("file is not UTF-8 text")]
    InvalidUtf8,
    #[error("file changed on disk; reload or resolve the conflict before saving")]
    Conflict,
    #[error("file is read-only")]
    ReadOnly,
    #[error("atomic replacement would break hard links")]
    HardLinks,
}

/// The last loaded or successfully saved contents of an existing file.
///
/// Share its immutable text with background tasks; keep edits and undo in the
/// editor buffer. A save advances this baseline only after replacement succeeds.
#[derive(Debug)]
pub struct DocumentFile {
    path: PathBuf,
    text: Arc<str>,
    limit: u64,
    read_only: bool,
}

impl DocumentFile {
    /// Resolves a new file's parent and rejects every existing destination, including symlinks.
    /// Publication must still use no-clobber semantics because another process may create it.
    pub fn new_path(path: &Path) -> Result<PathBuf, FileError> {
        let name = path.file_name().ok_or(FileError::NotRegular)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let path = fs::canonicalize(parent)?.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "Save As requires a new file name; destination already exists",
                )
                .into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(path)
    }

    /// Publishes a new file without replacing any existing entry, then moves this baseline.
    /// Accepts a resolved path from `new_path`; rejects parent resolution changes.
    /// The original stays untouched. New files use the temporary file's restrictive permissions.
    pub fn save_as(&mut self, path: &Path, text: Arc<str>) -> Result<(), FileError> {
        if text.len() as u64 > self.limit {
            return Err(FileError::TooLarge(self.limit));
        }
        let resolved = Self::new_path(path)?;
        if resolved != path {
            return Err(FileError::Conflict);
        }
        let path = resolved;
        let mut temporary =
            tempfile::NamedTempFile::new_in(path.parent().expect("resolved parent"))?;
        temporary.write_all(text.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(&path)
            .map_err(|error| error.error)?;
        self.path = path;
        self.text = text;
        self.read_only = false;
        Ok(())
    }

    /// Loads a regular UTF-8 file, resolving symlinks to a stable absolute path.
    /// Reads at most one byte beyond `limit`, including if the file grows.
    pub fn open(path: impl AsRef<Path>, limit: u64) -> Result<Self, FileError> {
        let path = fs::canonicalize(path)?;
        let (text, metadata) = read(&path, limit)?;
        Ok(Self {
            path,
            text,
            limit,
            read_only: metadata.permissions().readonly(),
        })
    }

    /// The resolved file path. Saving through an alias updates this target.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the file's permission bits forbade writing when it was loaded.
    /// Save independently checks current permissions and filesystem access.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Shares the exact loaded or saved UTF-8 contents without copying them.
    pub fn text(&self) -> Arc<str> {
        Arc::clone(&self.text)
    }

    /// Replaces the file if its contents still match this snapshot.
    ///
    /// Uses a temporary file in the same directory, flushes its contents, and
    /// preserves ownership, permissions and extended attributes on Linux, including
    /// access ACLs. Fails if metadata cannot be preserved. Other platforms reject
    /// replacement until their metadata handling is implemented. Rejects symlink
    /// replacement, read-only files and Unix hard links.
    ///
    /// Checks disk contents again immediately before atomic replacement. An
    /// uncooperative writer can still race that check; this is not a filesystem
    /// compare-and-swap. Callers must serialize saves for the same document.
    pub fn save(&mut self, text: Arc<str>) -> Result<(), FileError> {
        if text.len() as u64 > self.limit {
            return Err(FileError::TooLarge(self.limit));
        }
        let metadata = self.check_current()?;
        if text == self.text {
            return Ok(());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(
            self.path.parent().expect("a regular file has a parent"),
        )?;
        temporary.write_all(text.as_bytes())?;
        preserve_metadata(&File::open(&self.path)?, temporary.as_file(), &metadata)?;
        temporary.as_file().sync_all()?;
        let current = self.check_current()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ) != (
                current.dev(),
                current.ino(),
                current.ctime(),
                current.ctime_nsec(),
            ) {
                return Err(FileError::Conflict);
            }
        }
        #[cfg(not(unix))]
        let _ = current;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        self.text = text;
        Ok(())
    }

    /// Verifies the saved baseline and rejects unsupported replacement targets.
    fn check_current(&self) -> Result<Metadata, FileError> {
        let (text, metadata) = read(&self.path, self.limit)?;
        if text != self.text {
            return Err(FileError::Conflict);
        }
        if metadata.permissions().readonly() {
            return Err(FileError::ReadOnly);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() > 1 {
                return Err(FileError::HardLinks);
            }
        }
        Ok(metadata)
    }
}

/// Copies metadata before publication; failure leaves the original file untouched.
#[cfg(target_os = "linux")]
fn preserve_metadata(source: &File, target: &File, metadata: &Metadata) -> std::io::Result<()> {
    use rustix::fs::{
        Gid, Uid, XattrFlags, fchown, fgetxattr, flistxattr, fremovexattr, fsetxattr,
    };
    use std::os::unix::fs::MetadataExt;
    /// Bounds both individual attribute buffers and total retained values.
    fn attributes(file: &File) -> std::io::Result<std::collections::BTreeMap<Vec<u8>, Vec<u8>>> {
        let mut names = vec![0; 65536];
        let count = match flistxattr(file, names.as_mut_slice()) {
            Ok(count) => count,
            Err(rustix::io::Errno::NOTSUP) => return Ok(Default::default()),
            Err(error) => return Err(error.into()),
        };
        let mut attributes = std::collections::BTreeMap::new();
        let mut total = 0;
        for name in names[..count]
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
        {
            let mut value = vec![0; 65536];
            let count = fgetxattr(file, name, value.as_mut_slice())?;
            total += count;
            if total > 1024 * 1024 {
                return Err(std::io::Error::other(
                    "file attributes exceed the 1 MiB limit",
                ));
            }
            value.truncate(count);
            attributes.insert(name.to_vec(), value);
        }
        Ok(attributes)
    }
    let source_attributes = attributes(source)?;
    let target_metadata = target.metadata()?;
    if (metadata.uid(), metadata.gid()) != (target_metadata.uid(), target_metadata.gid()) {
        fchown(
            target,
            Some(Uid::from_raw(metadata.uid())),
            Some(Gid::from_raw(metadata.gid())),
        )?;
    }
    target.set_permissions(metadata.permissions())?;
    let target_attributes = attributes(target)?;
    for name in target_attributes.keys() {
        if !source_attributes.contains_key(name) {
            fremovexattr(target, name.as_slice())?;
        }
    }
    for (name, value) in source_attributes {
        if target_attributes.get(&name) != Some(&value) {
            fsetxattr(target, name.as_slice(), &value, XattrFlags::empty())?;
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn preserve_metadata(_: &File, _: &File, _: &Metadata) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "saving is currently supported only on Linux",
    ))
}

/// Checks the path before opening so an ordinary FIFO never blocks the worker.
fn read(path: &Path, limit: u64) -> Result<(Arc<str>, Metadata), FileError> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(FileError::NotRegular);
    }
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(FileError::NotRegular);
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(FileError::TooLarge(limit));
    }
    let text = String::from_utf8(bytes).map_err(|_| FileError::InvalidUtf8)?;
    Ok((Arc::from(text), metadata))
}
