//! Editing and save lifetimes against real files, without an editor toolkit.

use retrowork_platform::{document_file::DocumentFile, document_session::DocumentSession};
use ropey::Rope;
use std::fs;

fn session() -> (tempfile::TempDir, DocumentSession) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.rs");
    fs::write(&path, "original\r\n🦀").unwrap();
    let session = DocumentSession::new(DocumentFile::open(path, 128).unwrap());
    (directory, session)
}

#[test]
fn snapshots_share_the_session_and_undo_returns_to_the_saved_baseline() {
    let (_directory, session) = session();
    let original = session.text();
    let other_view = session.clone();
    assert_eq!(session, other_view);
    assert!(session.can_close().is_ok());
    assert!(session.save().is_err());
    session.edit(Rope::from("edited"));
    assert!(other_view.status().dirty);
    assert_eq!(other_view.text(), "edited");
    assert_eq!(original, "original\r\n🦀");
    assert!(
        session
            .can_close()
            .unwrap_err()
            .to_string()
            .contains("Unsaved")
    );
    session.edit(original);
    assert!(!session.status().dirty);
    assert!(session.can_close().is_ok());
    let (_, separate) = self::session();
    assert_ne!(session, separate);
}

#[test]
fn saving_an_older_snapshot_does_not_erase_new_edits() {
    let (directory, session) = session();
    session.edit(Rope::from("first edit"));
    let task = session.save().unwrap();
    assert!(session.status().saving);
    assert!(session.save().is_err());
    assert!(
        session
            .can_close()
            .unwrap_err()
            .to_string()
            .contains("running")
    );
    session.edit(Rope::from("later edit"));
    std::thread::spawn(move || task.run()).join().unwrap();
    assert_eq!(
        fs::read_to_string(directory.path().join("source.rs")).unwrap(),
        "first edit"
    );
    assert!(session.status().dirty);
    assert!(!session.status().saving);
    assert_eq!(session.text(), "later edit");
    session.save().unwrap().run();
    assert!(!session.status().dirty);
    assert!(session.can_close().is_ok());
    session.edit(Rope::from("first edit"));
    assert!(session.status().dirty);
    session.edit(Rope::from("later edit"));
    assert!(!session.status().dirty);
}

#[test]
fn failed_save_keeps_edits_and_reports_conflicts_without_overwriting_disk() {
    let (directory, session) = session();
    let path = directory.path().join("source.rs");
    session.edit(Rope::from("mine"));
    fs::write(&path, "external").unwrap();
    session.save().unwrap().run();
    assert_eq!(fs::read_to_string(&path).unwrap(), "external");
    let status = session.status();
    assert!(status.error.unwrap().contains("changed on disk"));
    assert!(status.dirty && !status.saving);
    assert_eq!(session.text(), "mine");
    assert!(session.can_close().is_err());
}

#[test]
fn unadmitted_work_can_be_retried_and_abandoned_sessions_do_not_write() {
    let (directory, session) = session();
    session.edit(Rope::from("mine"));
    drop(session.save().unwrap());
    assert!(session.status().error.unwrap().contains("cancelled"));
    assert!(!session.status().saving);
    session.save().unwrap().run();
    assert_eq!(session.status().error, None);
    let path = directory.path().join("source.rs");
    assert_eq!(fs::read_to_string(&path).unwrap(), "mine");
    session.edit(Rope::from("abandoned"));
    let task = session.save().unwrap();
    drop(session);
    task.run();
    assert_eq!(fs::read_to_string(path).unwrap(), "mine");
}

#[test]
fn oversized_edits_remain_available_after_a_save_failure() {
    let (directory, session) = session();
    session.edit(Rope::from("x".repeat(129)));
    session.save().unwrap().run();
    assert!(session.status().dirty);
    assert!(session.status().error.unwrap().contains("byte limit"));
    assert_eq!(session.text().len(), 129);
    assert_eq!(
        fs::read_to_string(directory.path().join("source.rs")).unwrap(),
        "original\r\n🦀"
    );
}

#[test]
fn save_as_keeps_later_edits_and_delivers_identity_only_when_completion_is_released() {
    let (directory, session) = session();
    let original = session.path();
    let copy = directory.path().join("copy.rs");
    let task = session.save_as(copy.clone()).unwrap();
    session.edit(Rope::from("later"));
    let completed = std::thread::spawn(move || task.run()).join().unwrap();
    assert!(completed.succeeded());
    assert!(session.status().saving);
    assert_eq!(session.path(), original);
    assert!(session.can_close().is_err());
    drop(completed);
    assert_eq!(session.path(), copy);
    assert!(session.status().dirty);
    assert_eq!(session.text(), "later");
    session.save().unwrap().run();
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "later");
    assert_eq!(std::fs::read_to_string(original).unwrap(), "original\r\n🦀");
}

#[test]
fn reload_advances_the_baseline_but_rejects_stale_reads_and_overlapping_writes() {
    let (directory, session) = session();
    let path = directory.path().join("source.rs");
    session.edit("mine".into());
    let revision = session.revision();
    fs::write(&path, "external").unwrap();
    session.edit("newer".into());
    assert!(
        session
            .reload(DocumentFile::open(&path, 128).unwrap(), revision)
            .is_err()
    );
    assert_eq!(session.text(), "newer");
    assert!(session.status().dirty);
    let revision = session.revision();
    let task = session.save().unwrap();
    assert!(
        session
            .reload(DocumentFile::open(&path, 128).unwrap(), revision)
            .is_err()
    );
    drop(task);
    let other = directory.path().join("other.rs");
    fs::write(&other, "other").unwrap();
    assert!(
        session
            .reload(DocumentFile::open(&other, 128).unwrap(), revision)
            .is_err()
    );
    let permissions = fs::metadata(&path).unwrap().permissions();
    let mut readonly = permissions.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&path, readonly).unwrap();
    assert!(
        session
            .reload(DocumentFile::open(&path, 128).unwrap(), revision)
            .is_err()
    );
    fs::set_permissions(&path, permissions).unwrap();
    session
        .reload(DocumentFile::open(&path, 128).unwrap(), revision)
        .unwrap();
    assert_eq!(session.text(), "external");
    assert!(!session.status().dirty);
    assert_eq!(session.status().error, None);
    assert!(session.revision() > revision);
    session.edit("after reload".into());
    session.save().unwrap().run();
    assert_eq!(session.status().error, None);
    assert_eq!(fs::read_to_string(path).unwrap(), "after reload");
}
