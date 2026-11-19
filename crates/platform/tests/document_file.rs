//! Document persistence contracts against real temporary files.

use std::{fs, path::Path, sync::Arc};

use retrowork_platform::document_file::{DocumentFile, FileError};

#[cfg(target_os = "linux")]
fn acl(path: &Path, rule: &str) {
    assert!(
        std::process::Command::new("setfacl")
            .args(["-m", rule])
            .arg(path)
            .status()
            .expect("setfacl is required for Linux metadata tests")
            .success()
    );
}

#[cfg(target_os = "linux")]
fn attribute(path: &Path, name: &str) -> Result<Vec<u8>, rustix::io::Errno> {
    let file = fs::File::open(path).unwrap();
    let mut bytes = vec![0; 65536];
    let count = rustix::fs::fgetxattr(&file, name, bytes.as_mut_slice())?;
    bytes.truncate(count);
    Ok(bytes)
}

#[cfg(target_os = "linux")]
#[test]
fn saving_preserves_access_acls_extended_attributes_and_ownership() {
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir().unwrap();
    let path = write(directory.path(), "script", "original");
    acl(&path, "u:12345:r--");
    let source = fs::File::open(&path).unwrap();
    rustix::fs::fsetxattr(
        &source,
        "user.retrowork-test",
        b"retained",
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    let metadata = source.metadata().unwrap();
    let original_acl = attribute(&path, "system.posix_acl_access").unwrap();
    let mut document = DocumentFile::open(&path, 128).unwrap();
    document.save(Arc::from("edited")).unwrap();
    let saved = fs::metadata(&path).unwrap();
    assert_eq!(
        (saved.uid(), saved.gid(), saved.mode()),
        (metadata.uid(), metadata.gid(), metadata.mode())
    );
    assert_eq!(
        attribute(&path, "system.posix_acl_access").unwrap(),
        original_acl
    );
    assert_eq!(
        attribute(&path, "user.retrowork-test").unwrap(),
        b"retained"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn replacement_does_not_inherit_new_directory_access_rules() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(directory.path(), "script", "original");
    let original_acl = attribute(&path, "system.posix_acl_access");
    acl(directory.path(), "d:u:12345:r--");
    let mut document = DocumentFile::open(&path, 128).unwrap();
    document.save(Arc::from("edited")).unwrap();
    assert_eq!(attribute(&path, "system.posix_acl_access"), original_acl);
}

fn write(directory: &Path, name: &str, text: impl AsRef<[u8]>) -> std::path::PathBuf {
    let path = directory.join(name);
    fs::write(&path, text).unwrap();
    path
}

#[test]
fn saving_preserves_unicode_line_endings_and_independent_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let original = "\u{feff}fn café() {}\r\n// 🦀\n";
    let path = write(directory.path(), "日本語.rs", original);
    let mut file = DocumentFile::open(&path, 1024).unwrap();
    let snapshot = file.text();
    assert_eq!(&*snapshot, original);
    assert!(Arc::ptr_eq(&snapshot, &file.text()));
    assert_eq!(file.path(), path.canonicalize().unwrap());
    let edited: Arc<str> = Arc::from("\u{feff}fn café() { }\r\n// 🦀\n");
    file.save(edited.clone()).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), &*edited);
    assert!(Arc::ptr_eq(&edited, &file.text()));
    assert_eq!(&*snapshot, original);
    file.save(Arc::from("next\n")).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "next\n");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn conflicting_external_edits_leave_both_baseline_and_disk_intact() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(directory.path(), "main.rs", "old");
    let mut file = DocumentFile::open(&path, 128).unwrap();
    fs::write(&path, "new").unwrap();
    for attempted in ["mine", "old"] {
        assert!(matches!(
            file.save(Arc::from(attempted)),
            Err(FileError::Conflict)
        ));
        assert_eq!(&*file.text(), "old");
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }
    let mut reloaded = DocumentFile::open(&path, 128).unwrap();
    reloaded.save(Arc::from("resolved")).unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "resolved");
}

#[test]
fn deleted_files_are_not_silently_recreated() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(directory.path(), "main.rs", "old");
    let mut file = DocumentFile::open(&path, 128).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(matches!(
        file.save(Arc::from("mine")),
        Err(FileError::Io(_))
    ));
    assert!(!path.exists());
    assert_eq!(&*file.text(), "old");
}

#[test]
fn read_and_save_limits_count_bytes_and_accept_the_exact_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(directory.path(), "main.rs", "🦀");
    assert!(matches!(
        DocumentFile::open(&path, 3),
        Err(FileError::TooLarge(3))
    ));
    let mut file = DocumentFile::open(&path, 4).unwrap();
    assert!(matches!(
        file.save(Arc::from("🦀!")),
        Err(FileError::TooLarge(4))
    ));
    assert_eq!(&*file.text(), "🦀");
    assert_eq!(fs::read_to_string(&path).unwrap(), "🦀");
    file.save(Arc::from("four")).unwrap();
    let empty = write(directory.path(), "empty", "");
    let mut file = DocumentFile::open(&empty, 0).unwrap();
    file.save(Arc::from("")).unwrap();
    assert!(matches!(
        file.save(Arc::from("x")),
        Err(FileError::TooLarge(0))
    ));
}

#[test]
fn invalid_or_nonregular_sources_are_rejected_without_lossy_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(directory.path(), "invalid", [0xff]);
    assert!(matches!(
        DocumentFile::open(&path, 128),
        Err(FileError::InvalidUtf8)
    ));
    assert!(matches!(
        DocumentFile::open(directory.path(), 128),
        Err(FileError::NotRegular)
    ));
    assert!(matches!(
        DocumentFile::open(directory.path().join("missing"), 128),
        Err(FileError::Io(_))
    ));
}

#[test]
fn invalid_or_oversized_external_changes_are_never_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(directory.path(), "main.rs", "old");
    let mut file = DocumentFile::open(&path, 4).unwrap();
    for bytes in [&[0xff][..], b"larger"] {
        fs::write(&path, bytes).unwrap();
        assert!(file.save(Arc::from("mine")).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(&*file.text(), "old");
    }
}

#[cfg(unix)]
#[test]
fn save_preserves_permission_bits_and_rejects_readonly_files() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let path = write(directory.path(), "script", "old");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
    let mut file = DocumentFile::open(&path, 128).unwrap();
    file.save(Arc::from("new")).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o750
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o440)).unwrap();
    assert!(DocumentFile::open(&path, 128).unwrap().is_read_only());
    assert!(matches!(
        file.save(Arc::from("mine")),
        Err(FileError::ReadOnly)
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    assert_eq!(&*file.text(), "new");
}

#[cfg(unix)]
#[test]
fn unchanged_saves_do_not_replace_the_file() {
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir().unwrap();
    let path = write(directory.path(), "main.rs", "old");
    let inode = fs::metadata(&path).unwrap().ino();
    let mut file = DocumentFile::open(&path, 128).unwrap();
    file.save(Arc::from("old")).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
}

#[cfg(unix)]
#[test]
fn symlink_aliases_resolve_on_open_and_replaced_targets_are_rejected() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let path = write(directory.path(), "main.rs", "old");
    let alias = directory.path().join("alias");
    symlink(&path, &alias).unwrap();
    let mut file = DocumentFile::open(&alias, 128).unwrap();
    file.save(Arc::from("new")).unwrap();
    assert!(fs::symlink_metadata(alias).unwrap().is_symlink());
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    let other = write(directory.path(), "other", "new");
    fs::remove_file(&path).unwrap();
    symlink(&other, &path).unwrap();
    assert!(matches!(
        file.save(Arc::from("mine")),
        Err(FileError::NotRegular)
    ));
    assert_eq!(fs::read_to_string(other).unwrap(), "new");
}

#[cfg(unix)]
#[test]
fn saving_does_not_silently_sever_hard_links() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(directory.path(), "main.rs", "old");
    let alias = directory.path().join("alias");
    fs::hard_link(&path, &alias).unwrap();
    let mut file = DocumentFile::open(&path, 128).unwrap();
    assert!(matches!(
        file.save(Arc::from("new")),
        Err(FileError::HardLinks)
    ));
    assert_eq!(fs::read_to_string(alias).unwrap(), "old");
    assert_eq!(fs::read_to_string(path).unwrap(), "old");
}

#[test]
fn save_as_publishes_a_new_file_without_changing_the_original() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original.rs");
    std::fs::write(&original, "original").unwrap();
    let mut file = DocumentFile::open(&original, 64).unwrap();
    let destination = DocumentFile::new_path(&directory.path().join("copy.rs")).unwrap();
    file.save_as(&destination, std::sync::Arc::from("changed\r\n🦀"))
        .unwrap();
    assert_eq!(std::fs::read_to_string(&original).unwrap(), "original");
    assert_eq!(
        std::fs::read_to_string(&destination).unwrap(),
        "changed\r\n🦀"
    );
    assert_eq!(file.path(), destination);
    file.save(std::sync::Arc::from("next")).unwrap();
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), "next");
    assert_eq!(std::fs::read_to_string(&original).unwrap(), "original");
}

#[test]
fn save_as_refuses_existing_entries_and_failed_creation_preserves_the_baseline() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original.rs");
    std::fs::write(&original, "original").unwrap();
    let mut file = DocumentFile::open(&original, 8).unwrap();
    let destination = DocumentFile::new_path(&directory.path().join("copy.rs")).unwrap();
    std::fs::write(&destination, "external").unwrap();
    assert!(
        file.save_as(&destination, std::sync::Arc::from("mine"))
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), "external");
    assert_eq!(file.path(), original);
    assert_eq!(&*file.text(), "original");
    assert!(DocumentFile::new_path(directory.path()).is_err());
    assert!(DocumentFile::new_path(&directory.path().join("missing/copy.rs")).is_err());
    let fresh = DocumentFile::new_path(&directory.path().join("too-large.rs")).unwrap();
    assert!(
        file.save_as(&fresh, std::sync::Arc::from("too large"))
            .is_err()
    );
    assert!(!fresh.exists());
    #[cfg(unix)]
    {
        let link = directory.path().join("link.rs");
        std::os::unix::fs::symlink("missing.rs", &link).unwrap();
        assert!(file.save_as(&link, std::sync::Arc::from("mine")).is_err());
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    }
}
