//! A single background writer with coalesced snapshots and explicit flushes.

use crate::{Inbox, PersistedState, StateStore};
use std::{
    io,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

/// A cloneable handle for waiting on persistence without borrowing the UI.
#[derive(Clone)]
pub struct Persistence {
    shared: Arc<(Mutex<State>, Condvar)>,
}

/// Only the newest requested snapshot is retained alongside save progress.
struct State {
    desired: Option<(u64, PersistedState)>,
    saved: u64,
    attempts: u64,
    error: Option<String>,
    retry_at: Instant,
    accepting: bool,
    stopping: bool,
}

impl Persistence {
    pub(crate) fn start(store: StateStore, inbox: Inbox) -> io::Result<Self> {
        Self::with_writer(
            move |state| store.save(state).map(|_| ()),
            move || {
                let _ = inbox.post_blocking(Box::new(|_| {}));
            },
        )
    }

    /// Starts the only writer; its callback is also replaceable in deterministic tests.
    fn with_writer(
        write: impl Fn(&PersistedState) -> io::Result<()> + Send + 'static,
        notify: impl Fn() + Send + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new((
            Mutex::new(State {
                desired: None,
                saved: 0,
                attempts: 0,
                error: None,
                retry_at: Instant::now(),
                accepting: true,
                stopping: false,
            }),
            Condvar::new(),
        ));
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("state-writer".into())
            .spawn(move || {
                let mut failures = 0u32;
                loop {
                    let (revision, snapshot) = {
                        let mut state = worker.0.lock().unwrap();
                        loop {
                            if state.stopping {
                                return;
                            }
                            if let Some((revision, snapshot)) = &state.desired
                                && *revision > state.saved
                                && Instant::now() >= state.retry_at
                            {
                                break (*revision, snapshot.clone());
                            }
                            state = if state
                                .desired
                                .as_ref()
                                .is_some_and(|(revision, _)| *revision > state.saved)
                            {
                                let delay =
                                    state.retry_at.saturating_duration_since(Instant::now());
                                worker.1.wait_timeout(state, delay).unwrap().0
                            } else {
                                worker.1.wait(state).unwrap()
                            };
                        }
                    };
                    let result = write(&snapshot);
                    {
                        let mut state = worker.0.lock().unwrap();
                        state.attempts += 1;
                        match result {
                            Ok(()) => {
                                state.saved = revision;
                                state.error = None;
                                failures = 0;
                                state.retry_at = Instant::now();
                            }
                            Err(error) => {
                                failures = (failures + 1).min(8);
                                state.error = Some(error.to_string());
                                state.retry_at = Instant::now()
                                    + Duration::from_millis(100 * (1u64 << (failures - 1)));
                            }
                        }
                        worker.1.notify_all();
                    }
                    notify();
                }
            })?;
        Ok(Self { shared })
    }

    /// Coalesces a changed snapshot; a failed snapshot remains dirty and is retried.
    pub(crate) fn submit(&self, snapshot: PersistedState) {
        let mut state = self.shared.0.lock().unwrap();
        if !state.accepting
            || state.stopping
            || state
                .desired
                .as_ref()
                .is_some_and(|(_, old)| old == &snapshot)
        {
            return;
        }
        let revision = state
            .desired
            .as_ref()
            .map_or(1, |(revision, _)| revision + 1);
        state.desired = Some((revision, snapshot));
        self.shared.1.notify_all();
    }

    pub(crate) fn error(&self) -> Option<String> {
        self.shared.0.lock().unwrap().error.clone()
    }

    /// Waits for the newest requested snapshot or one new failed attempt.
    /// Never call this on the UI thread during normal interaction.
    pub fn flush(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.0.lock().unwrap();
        let revision = state.desired.as_ref().map_or(0, |(revision, _)| *revision);
        let attempts = state.attempts;
        state.retry_at = Instant::now();
        self.shared.1.notify_all();
        loop {
            if state.saved >= revision {
                return Ok(());
            }
            if state.attempts > attempts
                && let Some(error) = &state.error
            {
                return Err(error.clone());
            }
            if state.stopping {
                return Err("persistence has stopped".into());
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err("timed out waiting for state persistence".into());
            };
            state = self.shared.1.wait_timeout(state, remaining).unwrap().0;
        }
    }

    /// Rejects new snapshots, flushes pending state, and stops further writes.
    /// A filesystem call already in progress may outlive the timeout.
    pub fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        self.shared.0.lock().unwrap().accepting = false;
        let result = self.flush(timeout);
        self.stop();
        result
    }

    /// Prevents further writes after the current filesystem call returns.
    pub(crate) fn stop(&self) {
        self.shared.0.lock().unwrap().stopping = true;
        self.shared.1.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    fn snapshot(name: &str) -> PersistedState {
        let mut state = PersistedState::default();
        state.active_views.insert("left".into(), name.into());
        state
    }

    #[test]
    fn blocked_writes_do_not_block_submission_and_only_the_latest_pending_state_is_written() {
        let (started, starts) = mpsc::channel();
        let (release, releases) = mpsc::channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        let values = Arc::clone(&written);
        let writer = Persistence::with_writer(
            move |state| {
                started.send(()).unwrap();
                releases.recv().unwrap();
                values.lock().unwrap().push(state.clone());
                Ok(())
            },
            || {},
        )
        .unwrap();
        writer.submit(snapshot("first"));
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
        writer.submit(snapshot("middle"));
        writer.submit(snapshot("last"));
        assert!(
            writer
                .flush(Duration::from_millis(10))
                .unwrap_err()
                .contains("timed out")
        );
        release.send(()).unwrap();
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
        release.send(()).unwrap();
        writer.flush(Duration::from_secs(2)).unwrap();
        assert_eq!(
            *written.lock().unwrap(),
            [snapshot("first"), snapshot("last")]
        );
        writer.stop();
    }

    #[test]
    fn a_failed_snapshot_can_be_retried_without_changing_it() {
        let fail = Arc::new(AtomicBool::new(true));
        let failing = Arc::clone(&fail);
        let writer = Persistence::with_writer(
            move |_| {
                if failing.load(Ordering::SeqCst) {
                    Err(io::Error::other("storage unavailable"))
                } else {
                    Ok(())
                }
            },
            || {},
        )
        .unwrap();
        writer.submit(snapshot("same"));
        assert!(writer.flush(Duration::from_secs(2)).is_err());
        fail.store(false, Ordering::SeqCst);
        writer.flush(Duration::from_secs(2)).unwrap();
        assert_eq!(writer.error(), None);
        writer.stop();
    }

    #[test]
    fn shutdown_times_out_without_starting_another_write() {
        let (started, starts) = mpsc::channel();
        let (release, releases) = mpsc::channel();
        let (finished, finishes) = mpsc::channel();
        let writer = Persistence::with_writer(
            move |_| {
                started.send(()).unwrap();
                releases.recv().unwrap();
                Ok(())
            },
            move || finished.send(()).unwrap(),
        )
        .unwrap();
        writer.submit(snapshot("active"));
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
        writer.submit(snapshot("pending"));
        assert!(
            writer
                .shutdown(Duration::from_millis(10))
                .unwrap_err()
                .contains("timed out")
        );
        writer.submit(snapshot("rejected"));
        release.send(()).unwrap();
        finishes.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            starts.recv_timeout(Duration::from_secs(2)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
        assert!(writer.flush(Duration::from_secs(2)).is_err());
    }
}
