//! Disposable file-finding experiment using existing matching and filesystem libraries.

pub mod ui;

use notify::{RecursiveMode, Watcher};
use nucleo::{
    Nucleo,
    pattern::{CaseMatching, Normalization},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

/// One open project and explicitly classified generated directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRoot {
    pub name: String,
    pub path: PathBuf,
    pub generated: Vec<PathBuf>,
}

/// A file retains project ownership even when another project has the same name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub project: String,
    pub root: PathBuf,
    pub relative: PathBuf,
    pub generated: bool,
    revision: u64,
}

/// Bounds on discovered entries and retained path text across all projects.
#[derive(Clone, Copy)]
pub struct Limits {
    pub entries: usize,
    pub path_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: 100_000,
            path_bytes: 32 * 1024 * 1024,
        }
    }
}

/// A bounded view of the latest completed query. Busy results cannot be opened.
#[derive(Clone, Debug, Default)]
pub struct Results {
    pub files: Vec<Candidate>,
    pub total: u32,
    pub matched: u32,
    pub busy: bool,
    pub truncated: bool,
    pub errors: Vec<String>,
}

#[derive(Clone, Default)]
struct Request {
    projects: Vec<ProjectRoot>,
    include_generated: bool,
    query: String,
}
struct Shared {
    request: Mutex<Request>,
    revision: AtomicU64,
    catalog: AtomicU64,
    stopped: AtomicBool,
    result: Mutex<(u64, Results)>,
}
struct Scan {
    revision: u64,
    matcher: Nucleo<Candidate>,
    errors: Vec<String>,
    truncated: bool,
}

/// Owns a bounded request mailbox. Filesystem work and Nucleo run off the caller thread.
/// Dropping requests cancellation without waiting on an operating-system file call.
pub struct Finder {
    shared: Arc<Shared>,
    wake: mpsc::SyncSender<()>,
}
impl Finder {
    /// Starts a worker; `changed` must cheaply schedule a UI refresh without blocking.
    pub fn new(limits: Limits, changed: Arc<dyn Fn() + Send + Sync>) -> std::io::Result<Self> {
        Self::start(limits, changed, true)
    }
    fn start(
        limits: Limits,
        changed: Arc<dyn Fn() + Send + Sync>,
        watch: bool,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            request: Mutex::new(Request::default()),
            revision: AtomicU64::new(0),
            catalog: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            result: Mutex::new((0, Results::default())),
        });
        let (wake, receive) = mpsc::sync_channel(1);
        let worker = shared.clone();
        let signal = wake.clone();
        thread::Builder::new()
            .name("finder-scan".into())
            .spawn(move || run(worker, receive, signal, limits, changed, watch))?;
        Ok(Self { shared, wake })
    }

    /// Replaces the open project set and immediately invalidates old candidates.
    pub fn set_projects(&mut self, projects: Vec<ProjectRoot>, include_generated: bool) {
        {
            let mut request = self.shared.request.lock().unwrap();
            request.projects = projects;
            request.include_generated = include_generated;
            self.shared.catalog.fetch_add(1, Ordering::AcqRel);
            self.shared.revision.fetch_add(1, Ordering::AcqRel);
        }
        let _ = self.wake.try_send(());
    }

    /// Rebuilds from disk, including after an undelivered filesystem notification.
    pub fn reconcile(&mut self) {
        self.shared.catalog.fetch_add(1, Ordering::AcqRel);
        self.shared.revision.fetch_add(1, Ordering::AcqRel);
        let _ = self.wake.try_send(());
    }

    /// Queues the latest smart-case fuzzy pattern; obsolete requests are coalesced.
    pub fn query(&mut self, text: &str) {
        {
            let mut request = self.shared.request.lock().unwrap();
            request.query = text.chars().take(1024).collect();
            self.shared.revision.fetch_add(1, Ordering::AcqRel);
        }
        let _ = self.wake.try_send(());
    }

    /// Copies at most 200 completed candidates; never waits for a matcher or file scan.
    pub fn poll(&mut self, limit: u32) -> Results {
        let result = self.shared.result.lock().unwrap();
        if result.0 != self.shared.revision.load(Ordering::Acquire) {
            return Results {
                busy: true,
                ..Results::default()
            };
        }
        let mut results = result.1.clone();
        results.files.truncate(limit as usize);
        results
    }

    /// Rejects obsolete candidates, removed files and escaping symlink replacements before opening.
    /// This final filesystem validation is synchronous and belongs on an opener worker in production.
    pub fn resolve(&mut self, candidate: &Candidate) -> Result<PathBuf, String> {
        let results = self.poll(200);
        if results.busy
            || !results.files.contains(candidate)
            || candidate.revision != self.shared.catalog.load(Ordering::Acquire)
        {
            return Err("Search results changed; select again".into());
        }
        let path = candidate.root.join(&candidate.relative);
        let canonical = path.canonicalize().map_err(|e| e.to_string())?;
        if !canonical.starts_with(&candidate.root) || !canonical.is_file() {
            return Err("File is no longer inside its project".into());
        }
        Ok(canonical)
    }
}
impl Drop for Finder {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        self.shared.catalog.fetch_add(1, Ordering::AcqRel);
        let _ = self.wake.try_send(());
    }
}

/// Serializes catalog replacement and query submission around the library-owned matcher.
fn run(
    shared: Arc<Shared>,
    receive: mpsc::Receiver<()>,
    wake: mpsc::SyncSender<()>,
    limits: Limits,
    changed: Arc<dyn Fn() + Send + Sync>,
    watch: bool,
) {
    let event_state = shared.clone();
    let signal = wake.clone();
    let watch_errors = Arc::new(Mutex::new(None));
    let errors = watch_errors.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let relevant = match event {
            Ok(event) => !matches!(event.kind, notify::EventKind::Access(_)),
            Err(error) => {
                *errors.lock().unwrap() = Some(error.to_string());
                true
            }
        };
        if relevant {
            event_state.catalog.fetch_add(1, Ordering::AcqRel);
            event_state.revision.fetch_add(1, Ordering::AcqRel);
            let _ = signal.try_send(());
        }
    });
    let matcher_changed: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = wake.try_send(());
    });
    let mut watched = Vec::new();
    let mut current: Option<Scan> = None;
    let mut query = String::new();
    let mut pending = false;
    loop {
        if pending {
            if matches!(
                receive.recv_timeout(std::time::Duration::from_millis(16)),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ) {
                break;
            }
        } else if receive.recv().is_err() {
            break;
        }
        if shared.stopped.load(Ordering::Acquire) {
            break;
        }
        let (request, revision, catalog) = {
            let request = shared.request.lock().unwrap();
            (
                request.clone(),
                shared.revision.load(Ordering::Acquire),
                shared.catalog.load(Ordering::Acquire),
            )
        };
        if current.as_ref().is_none_or(|scan| scan.revision != catalog) {
            let roots: Vec<_> = request.projects.iter().map(|p| p.path.clone()).collect();
            let mut errors = Vec::new();
            match &mut watcher {
                Ok(watcher) => {
                    watched.retain(|path| {
                        if roots.contains(path) {
                            true
                        } else {
                            if let Err(error) = watcher.unwatch(path) {
                                errors.push(error.to_string());
                            }
                            false
                        }
                    });
                    for path in roots
                        .iter()
                        .filter(|p| !watched.contains(p))
                        .cloned()
                        .collect::<Vec<_>>()
                    {
                        if watch {
                            match watcher.watch(&path, RecursiveMode::Recursive) {
                                Ok(()) => watched.push(path),
                                Err(error) => errors.push(error.to_string()),
                            }
                        }
                    }
                }
                Err(error) => errors.push(format!("Watcher unavailable: {error}")),
            }
            if let Some(error) = watch_errors.lock().unwrap().take() {
                errors.push(format!("Watcher lost updates; rebuilt file list: {error}"));
            }
            drop(current.take());
            let mut replacement = scan(&request, limits, catalog, &shared, matcher_changed.clone());
            replacement.errors.extend(errors);
            replacement.matcher.pattern.reparse(
                0,
                &request.query,
                CaseMatching::Smart,
                Normalization::Smart,
                false,
            );
            current = Some(replacement);
            query = request.query.clone();
        }
        let scan = current.as_mut().unwrap();
        if query != request.query {
            scan.matcher.pattern.reparse(
                0,
                &request.query,
                CaseMatching::Smart,
                Normalization::Smart,
                false,
            );
            query = request.query;
        }
        pending = scan.matcher.tick(0).running;
        if !pending && revision == shared.revision.load(Ordering::Acquire) {
            let snapshot = scan.matcher.snapshot();
            let results = Results {
                files: snapshot
                    .matched_items(..snapshot.matched_item_count().min(200))
                    .map(|i| i.data.clone())
                    .collect(),
                matched: snapshot.matched_item_count(),
                total: snapshot.item_count(),
                busy: false,
                truncated: scan.truncated,
                errors: scan.errors.clone(),
            };
            *shared.result.lock().unwrap() = (revision, results);
            changed();
        }
    }
}

/// Uses the library's ignore traversal and injects candidates on the directory worker.
fn scan(
    request: &Request,
    limits: Limits,
    revision: u64,
    shared: &Shared,
    changed: Arc<dyn Fn() + Send + Sync>,
) -> Scan {
    let matcher = Nucleo::new(nucleo::Config::DEFAULT.match_paths(), changed, Some(1), 1);
    let injector = matcher.injector();
    let mut scan = Scan {
        revision,
        matcher,
        errors: vec![],
        truncated: false,
    };
    let mut entries = 0;
    let mut bytes = 0;
    for project in &request.projects {
        let root = match project.path.canonicalize() {
            Ok(path) if path.is_dir() => path,
            _ => {
                scan.errors
                    .push(format!("Project unavailable: {}", project.path.display()));
                continue;
            }
        };
        for entry in ignore::WalkBuilder::new(&root)
            .hidden(false)
            .require_git(false)
            .follow_links(false)
            .max_depth(Some(64))
            .filter_entry(|entry| {
                !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | ".retrowork" | "target")
                )
            })
            .build()
        {
            if shared.stopped.load(Ordering::Acquire)
                || shared.catalog.load(Ordering::Acquire) != revision
            {
                return scan;
            }
            entries += 1;
            if entries > limits.entries {
                scan.truncated = true;
                return scan;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    if scan.errors.len() < 8 {
                        scan.errors.push(error.to_string());
                    }
                    continue;
                }
            };
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                if entry.depth() == 64 && entry.file_type().is_some_and(|kind| kind.is_dir()) {
                    scan.truncated = true;
                }
                continue;
            }
            let relative = entry.path().strip_prefix(&root).unwrap().to_owned();
            let generated = project
                .generated
                .iter()
                .any(|path| relative.starts_with(path));
            if generated && !request.include_generated {
                continue;
            }
            let label = format!("{} {}", relative.display(), project.name);
            bytes += label.len() + root.as_os_str().len() + relative.as_os_str().len();
            if bytes > limits.path_bytes {
                scan.truncated = true;
                return scan;
            }
            injector.push(
                Candidate {
                    project: project.name.clone(),
                    root: root.clone(),
                    relative,
                    generated,
                    revision,
                },
                |_, columns| {
                    columns[0] = label.as_str().into();
                },
            );
        }
    }
    scan
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{Duration, Instant},
    };

    fn project(path: &std::path::Path, name: &str) -> ProjectRoot {
        ProjectRoot {
            name: name.into(),
            path: path.into(),
            generated: vec!["generated".into()],
        }
    }
    fn write(root: &std::path::Path, name: &str) {
        let file = root.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "source").unwrap();
    }
    fn finder(watch: bool) -> Finder {
        Finder::start(Limits::default(), Arc::new(|| {}), watch).unwrap()
    }
    fn wait(finder: &mut Finder, condition: impl Fn(&Results) -> bool) -> Results {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let result = finder.poll(200);
            if !result.busy && condition(&result) {
                return result;
            }
            assert!(Instant::now() < deadline, "last results: {result:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn fuzzy_paths_duplicate_names_unicode_and_smart_case() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        write(a.path(), "src/ProjectWorkspace.rs");
        write(b.path(), "src/ProjectWorkspace.rs");
        write(a.path(), "docs/café.rs");
        let mut finder = finder(false);
        finder.set_projects(
            vec![project(a.path(), "alpha"), project(b.path(), "beta")],
            false,
        );
        wait(&mut finder, |r| r.total == 3);
        finder.query("s/PW");
        let results = wait(&mut finder, |r| r.matched == 2);
        assert_ne!(results.files[0].root, results.files[1].root);
        finder.query("PW beta");
        assert_eq!(
            wait(&mut finder, |r| r.matched == 1).files[0].project,
            "beta"
        );
        finder.query("café");
        assert_eq!(
            wait(&mut finder, |r| r.matched == 1).files[0].relative,
            PathBuf::from("docs/café.rs")
        );
        finder.query("CAFÉ");
        wait(&mut finder, |r| r.matched == 0);
    }
    #[test]
    fn ignore_rules_generated_policy_and_symlinks() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "src/main.rs");
        write(root.path(), "generated/model.rs");
        write(root.path(), "ignored.txt");
        write(root.path(), "target/artifact");
        write(root.path(), ".git/config");
        fs::write(root.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::os::unix::fs::symlink("src/main.rs", root.path().join("link.rs")).unwrap();
        let mut finder = finder(false);
        finder.set_projects(vec![project(root.path(), "alpha")], false);
        let r = wait(&mut finder, |r| r.total == 2);
        assert!(
            r.files
                .iter()
                .all(|f| !f.generated && f.relative != std::path::Path::new("link.rs"))
        );
        finder.set_projects(vec![project(root.path(), "alpha")], true);
        assert!(
            wait(&mut finder, |r| r.total == 3)
                .files
                .iter()
                .any(|f| f.generated)
        );
    }
    #[test]
    fn watcher_handles_create_rename_delete_and_ignore_changes() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "first.rs");
        let mut finder = finder(true);
        finder.set_projects(vec![project(root.path(), "alpha")], false);
        wait(&mut finder, |r| r.total == 1);
        write(root.path(), "second.rs");
        wait(&mut finder, |r| r.total == 2);
        fs::rename(
            root.path().join("second.rs"),
            root.path().join("renamed.rs"),
        )
        .unwrap();
        wait(&mut finder, |r| {
            r.files
                .iter()
                .any(|f| f.relative == std::path::Path::new("renamed.rs"))
        });
        fs::remove_file(root.path().join("first.rs")).unwrap();
        wait(&mut finder, |r| r.total == 1);
        fs::write(root.path().join(".ignore"), "renamed.rs\n").unwrap();
        let r = wait(&mut finder, |r| {
            r.files
                .iter()
                .all(|f| f.relative != std::path::Path::new("renamed.rs"))
        });
        assert_eq!(r.total, 1);
    }
    #[test]
    fn reconciliation_recovers_changes_without_notifications() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "old.rs");
        let mut finder = finder(false);
        finder.set_projects(vec![project(root.path(), "alpha")], false);
        let stale = wait(&mut finder, |r| r.total == 1).files[0].clone();
        fs::rename(root.path().join("old.rs"), root.path().join("new.rs")).unwrap();
        assert!(finder.resolve(&stale).is_err());
        finder.reconcile();
        let r = wait(&mut finder, |r| r.total == 1);
        assert_eq!(r.files[0].relative, PathBuf::from("new.rs"));
        assert!(finder.resolve(&stale).is_err());
        assert!(finder.resolve(&r.files[0]).is_ok());
    }
    #[test]
    fn project_closure_and_rapid_queries_reject_stale_results() {
        let root = tempfile::tempdir().unwrap();
        for n in 0..500 {
            write(root.path(), &format!("file{n}.rs"));
        }
        let mut finder = finder(false);
        finder.set_projects(vec![project(root.path(), "alpha")], false);
        wait(&mut finder, |r| r.total == 500);
        for query in ["file1", "file33", "nonsense", "file499"] {
            finder.query(query);
        }
        let stale = wait(&mut finder, |r| r.matched == 1).files[0].clone();
        finder.set_projects(vec![], false);
        assert!(finder.resolve(&stale).is_err());
        wait(&mut finder, |r| r.total == 0);
        finder.set_projects(vec![project(root.path(), "alpha")], false);
        finder.set_projects(vec![], false);
        wait(&mut finder, |r| r.total == 0);
    }
    #[test]
    fn background_matching_completes_empty_and_nonempty_transitions() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "file.rs");
        let mut finder = finder(false);
        finder.query("file");
        for _ in 0..30 {
            finder.set_projects(vec![project(root.path(), "alpha")], false);
            wait(&mut finder, |r| r.matched == 1);
            finder.set_projects(vec![], false);
            wait(&mut finder, |r| r.total == 0);
        }
    }
    #[test]
    fn bounds_and_missing_projects_are_visible() {
        let root = tempfile::tempdir().unwrap();
        for n in 0..20 {
            write(root.path(), &format!("file{n}.rs"));
        }
        for limits in [
            Limits {
                entries: 4,
                path_bytes: 10000,
            },
            Limits {
                entries: 100,
                path_bytes: 1,
            },
        ] {
            let mut finder = Finder::start(limits, Arc::new(|| {}), false).unwrap();
            finder.set_projects(vec![project(root.path(), "alpha")], false);
            assert!(wait(&mut finder, |_| true).truncated);
        }
        let mut finder = finder(false);
        finder.set_projects(vec![project(&root.path().join("missing"), "gone")], false);
        assert!(!wait(&mut finder, |_| true).errors.is_empty());
    }
    #[test]
    fn dropping_finder_releases_the_worker_and_watchers() {
        let root = tempfile::tempdir().unwrap();
        for n in 0..500 {
            write(root.path(), &format!("file{n}.rs"));
        }
        let mut finder = finder(true);
        let worker = Arc::downgrade(&finder.shared);
        finder.set_projects(vec![project(root.path(), "alpha")], false);
        drop(finder);
        let deadline = Instant::now() + Duration::from_secs(10);
        while worker.upgrade().is_some() {
            assert!(Instant::now() < deadline, "worker leaked");
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn real_workspace_paths_are_found_without_build_output() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut finder = finder(false);
        finder.set_projects(vec![project(&root, "RetroWork")], false);
        finder.query("cr/pl/src/lib");
        let results = wait(&mut finder, |r| r.matched > 0);
        assert!(
            results
                .files
                .iter()
                .any(|file| file.relative == std::path::Path::new("crates/platform/src/lib.rs"))
        );
        assert!(!results.truncated);
        assert!(results.errors.is_empty(), "{:?}", results.errors);
    }
}
