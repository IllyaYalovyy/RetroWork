//! Typed services shared between modules, and the inbox background work
//! reports through.

use std::{
    any::{Any, TypeId},
    cell::RefCell,
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

use crate::{ModuleId, command::CommandContext};

/// Services registered by active modules, looked up by type.
///
/// A module that offers an API to others registers an `Rc<T>` here; dependents
/// name the type, so the dependency is visible to the compiler as well as in
/// the manifest.
#[derive(Default)]
pub struct Services {
    map: HashMap<TypeId, Service>,
    order: Vec<TypeId>,
    resources: RefCell<Vec<(ModuleId, Rc<dyn ModuleResource>)>>,
    dependencies: HashMap<ModuleId, Vec<ModuleId>>,
    inbox: Inbox,
}

impl Services {
    /// The service of type `T`, if an active module provides one.
    pub fn get<T: 'static>(&self) -> Option<Rc<T>> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|service| Rc::clone(&service.value).downcast::<T>().ok())
    }

    pub(crate) fn register_module(&mut self, manifest: &crate::ModuleManifest) {
        self.dependencies.insert(
            manifest.id.clone(),
            manifest
                .dependencies
                .iter()
                .map(|dependency| dependency.id.clone())
                .collect(),
        );
    }

    pub(crate) fn access<'a>(&'a self, owner: Option<&'a ModuleId>) -> ServiceAccess<'a> {
        ServiceAccess {
            services: self,
            owner,
        }
    }

    pub(crate) fn contains(&self, type_id: TypeId) -> bool {
        self.map.contains_key(&type_id)
    }

    /// Stores `service` under `type_id`, which must be the `TypeId` of the
    /// value inside the `Rc`; [`Self::get`] relies on that to downcast.
    pub(crate) fn insert_erased(&mut self, owner: ModuleId, type_id: TypeId, service: Rc<dyn Any>) {
        self.map.insert(
            type_id,
            Service {
                owner,
                value: service,
            },
        );
        self.order.push(type_id);
    }

    /// The module that owns a service. Host code may inspect all services.
    pub fn owner<T: 'static>(&self) -> Option<&ModuleId> {
        self.map
            .get(&TypeId::of::<T>())
            .map(|service| &service.owner)
    }

    pub(crate) fn manage(&self, owner: ModuleId, resource: Rc<dyn ModuleResource>) {
        self.resources.borrow_mut().push((owner, resource));
    }

    /// Stops owned resources in reverse activation order, exactly once.
    pub(crate) fn shutdown(&self) {
        let resources = std::mem::take(&mut *self.resources.borrow_mut());
        for (_, resource) in resources.into_iter().rev() {
            resource.shutdown();
        }
    }

    pub(crate) fn reader(&self) -> InboxReader<'_> {
        InboxReader(&self.inbox)
    }

    /// Where background threads post work that must run on the UI thread.
    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }
}

/// Service access restricted to the current module and its declared dependencies.
pub struct ServiceAccess<'a> {
    services: &'a Services,
    owner: Option<&'a ModuleId>,
}
impl ServiceAccess<'_> {
    /// Reads a service only when its owner is a declared dependency, self or the core.
    pub fn get<T: 'static>(&self) -> Option<Rc<T>> {
        let service_owner = self.services.owner::<T>()?;
        if let Some(owner) = self.owner
            && owner != service_owner
            && service_owner.as_str() != crate::core::CORE_MODULE_ID
            && !self
                .services
                .dependencies
                .get(owner)
                .is_some_and(|dependencies| dependencies.contains(service_owner))
        {
            return None;
        }
        self.services.get()
    }
    /// Creates a producer whose completions retain this module's service access.
    pub fn inbox(&self) -> Inbox {
        self.services.inbox.scoped(self.owner.cloned())
    }
}

/// A service with its activation owner.
struct Service {
    owner: ModuleId,
    value: Rc<dyn Any>,
}

/// A module-owned resource that needs explicit cancellation or disposal.
/// Shutdown must be short and nonblocking; cancel workers rather than joining them here.
pub trait ModuleResource {
    /// Stops accepting work and releases owned registrations or requests cancellation.
    fn shutdown(&self);
}

impl Drop for Services {
    fn drop(&mut self) {
        self.shutdown();
        for key in self.order.iter().rev() {
            self.map.remove(key);
        }
    }
}

/// Work posted from another thread, run later with full command access.
pub type Completion = Box<dyn FnOnce(&mut CommandContext<'_>) + Send>;

/// Maximum queued completions. Background producers wait when this is reached.
pub const INBOX_CAPACITY: usize = 256;

/// Why a completion could not be queued.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PostError {
    /// The platform has shut down.
    #[error("platform inbox is closed")]
    Closed,
    /// The UI thread cannot wait for itself to drain a full queue.
    #[error("platform inbox is full")]
    Full,
    /// Blocking submission was attempted from the UI thread.
    #[error("blocking submission is forbidden on the UI thread")]
    UiThread,
}

/// Rejected work remains owned by the caller and can be retried or explicitly dropped.
pub struct RejectedCompletion {
    pub reason: PostError,
    pub completion: Completion,
}
impl std::fmt::Debug for RejectedCompletion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RejectedCompletion")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl Default for InboxState {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            waker: None,
            owner: thread::current().id(),
            closed: false,
        }
    }
}

type Waker = Arc<dyn Fn() + Send + Sync>;

struct InboxState {
    queue: VecDeque<QueuedCompletion>,
    waker: Option<Waker>,
    owner: ThreadId,
    closed: bool,
}

/// A thread-safe queue of [`Completion`]s.
///
/// Module state is single-threaded. A background thread never touches it;
/// it posts a completion here and the platform runs it on the UI thread in
/// [`crate::Platform::pump`]. The front end installs a waker so that posting
/// schedules a pump instead of requiring polling.
#[derive(Clone, Default)]
pub struct Inbox {
    scope: Option<ModuleId>,
    state: Arc<(Mutex<InboxState>, Condvar)>,
}

/// Accepted work carries its contributing module through the worker boundary.
pub(crate) struct QueuedCompletion {
    pub(crate) owner: Option<ModuleId>,
    pub(crate) apply: Completion,
}

impl Inbox {
    fn scoped(&self, scope: Option<ModuleId>) -> Self {
        Self {
            state: Arc::clone(&self.state),
            scope,
        }
    }

    fn lock(&self) -> MutexGuard<'_, InboxState> {
        // A panic while holding the lock cannot leave the queue inconsistent.
        self.state.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues work immediately or returns it unchanged when the queue is full or closed.
    pub fn try_post(&self, completion: Completion) -> Result<(), RejectedCompletion> {
        self.submit(completion, false)
    }

    /// Waits for capacity on a worker thread. Shutdown returns the unqueued work.
    /// Calling this on the creating UI thread always fails, even when capacity is available.
    pub fn post_blocking(&self, completion: Completion) -> Result<(), RejectedCompletion> {
        self.submit(completion, true)
    }

    fn submit(&self, completion: Completion, blocking: bool) -> Result<(), RejectedCompletion> {
        let waker = {
            let mut state = self.lock();
            if blocking && thread::current().id() == state.owner {
                return Err(RejectedCompletion {
                    reason: PostError::UiThread,
                    completion,
                });
            }
            while !state.closed && state.queue.len() == INBOX_CAPACITY {
                if !blocking {
                    return Err(RejectedCompletion {
                        reason: PostError::Full,
                        completion,
                    });
                }
                state = self
                    .state
                    .1
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if state.closed {
                return Err(RejectedCompletion {
                    reason: PostError::Closed,
                    completion,
                });
            }
            let wake = state.queue.is_empty();
            state.queue.push_back(QueuedCompletion {
                owner: self.scope.clone(),
                apply: completion,
            });
            if wake { state.waker.clone() } else { None }
        };
        self.state.1.notify_all();
        if let Some(waker) = waker {
            waker();
        }
        Ok(())
    }
}

/// Front-end access to queue draining, wakeups and closure. Never given to plugins.
pub(crate) struct InboxReader<'a>(&'a Inbox);
impl InboxReader<'_> {
    fn lock(&self) -> MutexGuard<'_, InboxState> {
        self.0.lock()
    }

    /// Installs the front-end wake callback and signals any pending work.
    pub(crate) fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        let waker: Waker = Arc::new(waker);
        let pending = {
            let mut state = self.lock();
            state.waker = Some(Arc::clone(&waker));
            !state.queue.is_empty()
        };
        if pending {
            waker();
        }
    }

    /// Takes one completion and releases one waiting producer.
    pub(crate) fn pop(&self) -> Option<QueuedCompletion> {
        let completion = self.lock().queue.pop_front();
        self.0.state.1.notify_all();
        completion
    }

    /// Schedules another UI turn when the current turn left work queued.
    pub(crate) fn wake_pending(&self) {
        let waker = {
            let state = self.lock();
            if state.queue.is_empty() {
                None
            } else {
                state.waker.clone()
            }
        };
        if let Some(waker) = waker {
            waker();
        }
    }

    /// Stops admission and drops queued work outside the queue lock.
    pub(crate) fn close(&self) {
        let pending = {
            let mut state = self.lock();
            state.closed = true;
            state.waker = None;
            std::mem::take(&mut state.queue)
        };
        self.0.state.1.notify_all();
        drop(pending);
    }

    /// Blocks until something is queued or `timeout` passes. For front ends
    /// without an event loop, such as tests and command-line tools.
    pub(crate) fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        while state.queue.is_empty() && !state.closed {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            state = self
                .0
                .state
                .1
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        !state.queue.is_empty()
    }
}
