//! The running platform: resolved modules, their contributions and the shell.

use std::{
    cell::Cell,
    collections::BTreeMap,
    rc::Rc,
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::{
    activation::ActivationContext,
    command::{CommandContext, CommandError},
    contribution::{Contributions, ViewLocation},
    core::CoreModule,
    id::{CommandId, DocumentKind, ViewId},
    model::{Tone, ViewContent, ViewEvent},
    module::{
        Module, ModuleDirectory, ModuleReport, ModuleState, PLATFORM_API_VERSION, RejectReason,
    },
    resolve::resolve,
    services::Services,
    shell::{Document, DocumentId, Shell, ShellRequest},
    state::{LoadOutcome, PersistedDocument, PersistedState, STATE_SCHEMA_VERSION, StateStore},
};

/// Most commands one user action may chain through `Shell::run_command`.
const MAX_CHAINED_REQUESTS: usize = 32;

/// What a view event is aimed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewTarget {
    /// A docked view.
    View(ViewId),
    /// An open document, independent of its tab position.
    Document(DocumentId),
}

/// Collects modules and the state store, then builds a [`Platform`].
#[derive(Default)]
pub struct PlatformBuilder {
    modules: Vec<Box<dyn Module>>,
    store: Option<StateStore>,
}

impl PlatformBuilder {
    /// A builder with no modules and no state store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a module. Order does not affect the result, except that the
    /// first of two modules with the same id wins.
    pub fn module(mut self, module: impl Module + 'static) -> Self {
        self.modules.push(Box::new(module));
        self
    }

    /// Registers a module that is already boxed.
    pub fn boxed_module(mut self, module: Box<dyn Module>) -> Self {
        self.modules.push(module);
        self
    }

    /// Restores state from `store` after activation and saves to it after
    /// every change.
    pub fn state_store(mut self, store: StateStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Resolves dependencies, activates accepted modules in order and
    /// restores persisted state.
    pub fn build(self) -> Platform {
        let shell = Shell::default();
        let mut modules: Vec<Box<dyn Module>> = vec![Box::new(CoreModule {
            output: Rc::clone(shell.output()),
        })];
        modules.extend(self.modules);

        let manifests: Vec<_> = modules.iter().map(|module| module.manifest()).collect();
        let resolution = resolve(&manifests, &PLATFORM_API_VERSION);
        let mut first_registration = BTreeMap::new();
        for (index, manifest) in manifests.iter().enumerate() {
            first_registration.entry(&manifest.id).or_insert(index);
        }
        let mut states: BTreeMap<usize, ModuleState> = resolution
            .rejected
            .into_iter()
            .map(|(index, reason)| (index, ModuleState::Rejected(reason)))
            .collect();

        let directory = Rc::new(ModuleDirectory::default());
        let mut services = Services::default();
        services.insert_erased(
            crate::ModuleId::literal(crate::core::CORE_MODULE_ID),
            std::any::TypeId::of::<ModuleDirectory>(),
            Rc::clone(&directory) as Rc<dyn std::any::Any>,
        );
        let mut contributions = Contributions::default();

        // The core module (index 0) defines the menus and views every other
        // module may refer to, so it goes first whatever its id sorts as. It
        // has no dependencies, so moving it keeps the order valid.
        let mut order = resolution.order;
        order.sort_by_key(|&index| index != 0);
        for index in order {
            let manifest = &manifests[index];
            // A dependency can still fail during its own activation.
            let failed_dependency = manifest.dependencies.iter().find(|dependency| {
                matches!(
                    states.get(&first_registration[&dependency.id]),
                    Some(ModuleState::Rejected(_))
                )
            });
            if let Some(dependency) = failed_dependency {
                states.insert(
                    index,
                    ModuleState::Rejected(RejectReason::DependencyUnavailable(
                        dependency.id.clone(),
                    )),
                );
                continue;
            }

            let mut cx = ActivationContext::new(manifest, &contributions, &services);
            let outcome = modules[index]
                .activate(&mut cx)
                .and_then(|()| cx.validate());
            match outcome {
                Ok(commits) => {
                    let ActivationContext {
                        staged,
                        staged_services,
                        staged_registrations,
                        staged_resources,
                        ..
                    } = cx;
                    services.register_module(manifest);
                    contributions.merge(staged);
                    for (type_id, service) in staged_services {
                        services.insert_erased(manifest.id.clone(), type_id, service);
                    }
                    for resource in staged_resources {
                        services.manage(manifest.id.clone(), resource);
                    }
                    for commit in staged_registrations {
                        services.manage(manifest.id.clone(), commit());
                    }
                    for commit in commits {
                        commit();
                    }
                    states.insert(index, ModuleState::Active);
                }
                Err(error) => {
                    for resource in cx.staged_resources.drain(..).rev() {
                        resource.shutdown();
                    }
                    states.insert(
                        index,
                        ModuleState::Rejected(RejectReason::ActivationFailed(error.0)),
                    );
                }
            }
        }

        let reports: Vec<ModuleReport> = manifests
            .into_iter()
            .enumerate()
            .map(|(index, manifest)| ModuleReport {
                manifest,
                state: states
                    .remove(&index)
                    .expect("every module is accepted or rejected"),
            })
            .collect();
        directory.set(reports.clone());

        let mut platform = Platform {
            modules: reports,
            contributions,
            services,
            shell,
            store: self.store,
            persistence: None,
            last_save_error: None,
            shutting_down: Cell::new(false),
        };
        platform.report_modules();
        for location in [ViewLocation::Left, ViewLocation::Bottom] {
            if let Some(first) = platform.contributions.views(location).first() {
                let first = first.id.clone();
                platform.shell.set_active_view(location, first);
            }
        }
        platform.restore_state();
        if let Some(store) = platform.store.take()
            && !store.is_read_only()
        {
            match crate::Persistence::start(store, platform.services.inbox().clone()) {
                Ok(writer) => platform.persistence = Some(writer),
                Err(error) => platform
                    .shell
                    .output()
                    .append(format!("Cannot start state writer: {error}"), Tone::Error),
            }
        }
        platform
    }
}

/// A running application: active modules, their contributions and services,
/// and the shell state a front end draws.
pub struct Platform {
    modules: Vec<ModuleReport>,
    contributions: Contributions,
    services: Services,
    shell: Shell,
    store: Option<StateStore>,
    persistence: Option<crate::Persistence>,
    last_save_error: Option<String>,
    shutting_down: Cell<bool>,
}

impl Platform {
    /// A builder with no modules. The core module is always added when it builds.
    pub fn builder() -> PlatformBuilder {
        PlatformBuilder::new()
    }

    /// Every registered module, rejected ones included, core first.
    pub fn modules(&self) -> &[ModuleReport] {
        &self.modules
    }

    /// What the active modules contributed.
    pub fn contributions(&self) -> &Contributions {
        &self.contributions
    }

    /// The services modules offer each other, and the inbox.
    pub fn services(&self) -> &Services {
        &self.services
    }

    /// Documents, active views, status, output and trace.
    pub fn shell(&self) -> &Shell {
        &self.shell
    }

    /// For the front end: recording input in the trace, taking focus requests.
    pub fn shell_mut(&mut self) -> &mut Shell {
        &mut self.shell
    }

    fn report_modules(&mut self) {
        for report in &self.modules {
            match &report.state {
                ModuleState::Active => self
                    .shell
                    .trace_mut()
                    .record("module.activated", report.manifest.id.as_str()),
                ModuleState::Rejected(reason) => {
                    self.shell.trace_mut().record(
                        "module.rejected",
                        format!("{}: {reason}", report.manifest.id),
                    );
                    self.shell.output().append(
                        format!("Module `{}` was not loaded: {reason}.", report.manifest.id),
                        Tone::Warning,
                    );
                }
            }
        }
    }

    /// Runs `command` with the handler its module registered.
    ///
    /// Failures are returned and also made visible: an error line in the
    /// Output, the status text and a `command.failed` trace event.
    pub fn execute(
        &mut self,
        command: &CommandId,
        args: Option<&Value>,
    ) -> Result<(), CommandError> {
        let result = self
            .run_handler(command, args)
            .and_then(|()| self.drain_requests());
        match &result {
            Ok(()) => self
                .shell
                .trace_mut()
                .record("command.executed", command.as_str()),
            Err(error) => self.report_failure(&format!("Command `{command}` failed"), error),
        }
        self.save_if_changed();
        result
    }

    /// Returns why a command is unavailable, or `None` when it can run.
    /// Used by controls and checked again immediately before execution.
    pub fn command_disabled_reason(
        &self,
        command: &CommandId,
        args: Option<&Value>,
    ) -> Option<String> {
        if self.shutting_down.get() {
            return Some("platform is shutting down".into());
        }
        if self.contributions.command(command).is_none() {
            return Some(format!("unknown command `{command}`"));
        }
        self.contributions
            .commands
            .get(command)
            .and_then(|command| {
                command.availability.as_ref().and_then(|check| {
                    check(
                        &self.shell,
                        &self.services.access(Some(&command.spec.owner)),
                        args,
                    )
                    .err()
                })
            })
    }

    fn run_handler(
        &mut self,
        command: &CommandId,
        args: Option<&Value>,
    ) -> Result<(), CommandError> {
        let (owner, handler) = self
            .contributions
            .commands
            .get(command)
            .map(|command| (command.spec.owner.clone(), Rc::clone(&command.handler)))
            .ok_or_else(|| CommandError::UnknownCommand(command.clone()))?;
        if let Some(reason) = self.command_disabled_reason(command, args) {
            return Err(CommandError::failed(reason));
        }
        handler(&mut CommandContext {
            shell: &mut self.shell,
            services: self.services.access(Some(&owner)),
            args,
        })
    }

    fn report_failure(&mut self, what: &str, error: &CommandError) {
        // Requests queued by a failed handler must not leak into the next action.
        self.shell.requests.clear();
        let message = format!("{what}: {error}");
        self.shell.trace_mut().record("command.failed", &message);
        self.shell.output().append(&message, Tone::Error);
        self.shell.set_status(message);
    }

    /// Carries out what handlers queued on the shell.
    fn drain_requests(&mut self) -> Result<(), CommandError> {
        let mut handled = 0;
        while let Some(request) = self.shell.requests.pop_front() {
            handled += 1;
            if handled > MAX_CHAINED_REQUESTS {
                return Err(CommandError::RunawayChain(MAX_CHAINED_REQUESTS));
            }
            match request {
                ShellRequest::DocumentEvent { document, event } => {
                    let Some(doc) = self.shell.document(document) else {
                        continue;
                    };
                    let provider = doc.provider.clone();
                    let owner = doc.owner.clone();
                    provider.handle(
                        event,
                        &mut CommandContext {
                            shell: &mut self.shell,
                            services: self.services.access(Some(&owner)),
                            args: None,
                        },
                    )?;
                }
                ShellRequest::OpenDocument {
                    kind,
                    resource,
                    event,
                } => {
                    let index = self.open_document(&kind, &resource)?;
                    if let Some(event) = event {
                        let provider = Rc::clone(&self.shell.documents()[index].provider);
                        let owner = self.shell.documents()[index].owner.clone();
                        provider.handle(
                            event,
                            &mut CommandContext {
                                shell: &mut self.shell,
                                services: self.services.access(Some(&owner)),
                                args: None,
                            },
                        )?;
                    }
                }
                ShellRequest::ShowView(view) => {
                    if !self.activate_view(&view) {
                        return Err(CommandError::Failed(format!("unknown view `{view}`")));
                    }
                }
                ShellRequest::RunCommand { command, args } => {
                    self.run_handler(&command, args.as_ref())?;
                    self.shell
                        .trace_mut()
                        .record("command.executed", command.as_str());
                }
            }
        }
        Ok(())
    }

    fn open_document(
        &mut self,
        kind: &DocumentKind,
        resource: &str,
    ) -> Result<usize, CommandError> {
        if let Some(index) = self.shell.find_document(kind, resource) {
            self.shell.activate_document(index);
            return Ok(index);
        }
        let (owner, factory) = self
            .contributions
            .document_factories
            .get(kind)
            .map(|(owner, factory)| (owner.clone(), Rc::clone(factory)))
            .ok_or_else(|| {
                CommandError::Failed(format!("no module can open documents of kind `{kind}`"))
            })?;
        let opened = factory.open(
            resource,
            &mut CommandContext {
                shell: &mut self.shell,
                services: self.services.access(Some(&owner)),
                args: None,
            },
        )?;
        let index = self.shell.push_document(Document {
            id: DocumentId(0),
            kind: kind.clone(),
            resource: resource.to_owned(),
            title: opened.title,
            provider: opened.provider,
            owner,
        });
        self.shell.activate_document(index);
        Ok(index)
    }

    /// Forwards a user interaction to the provider of a view or document.
    pub fn view_event(
        &mut self,
        target: &ViewTarget,
        event: ViewEvent,
    ) -> Result<(), CommandError> {
        if self.shutting_down.get() {
            return Err(CommandError::failed("platform is shutting down"));
        }
        let provider = match target {
            ViewTarget::View(view) => self.contributions.view(view).and_then(|spec| {
                self.contributions
                    .view_provider(view)
                    .map(|provider| (spec.owner.clone(), Rc::clone(provider)))
            }),
            ViewTarget::Document(id) => self
                .shell
                .document(*id)
                .map(|document| (document.owner.clone(), Rc::clone(&document.provider))),
        };
        let result = match provider {
            Some((owner, provider)) => provider
                .handle(
                    event,
                    &mut CommandContext {
                        shell: &mut self.shell,
                        services: self.services.access(Some(&owner)),
                        args: None,
                    },
                )
                .and_then(|()| self.drain_requests()),
            None => Err(CommandError::Failed(format!(
                "no such view target: {target:?}"
            ))),
        };
        if let Err(error) = &result {
            self.report_failure("View event failed", error);
        }
        self.save_if_changed();
        result
    }

    /// Current content of a docked view.
    pub fn view_content(&self, view: &ViewId) -> Option<ViewContent> {
        self.contributions
            .view_provider(view)
            .map(|provider| provider.content(&self.shell))
    }

    /// Current content of the identified open document.
    pub fn document_content(&self, id: DocumentId) -> Option<ViewContent> {
        self.shell
            .document(id)
            .map(|document| document.provider.content(&self.shell))
    }

    /// Brings a docked view to the front of its dock. Returns `false` for an
    /// unknown view.
    pub fn activate_view(&mut self, view: &ViewId) -> bool {
        if self.shutting_down.get() {
            return false;
        }
        let Some(location) = self.contributions.view(view).map(|spec| spec.location) else {
            return false;
        };
        self.shell.set_active_view(location, view.clone());
        self.save_if_changed();
        true
    }

    /// Activates an open document. A closed identity has no effect.
    pub fn activate_document(&mut self, id: DocumentId) {
        if self.shutting_down.get() {
            return;
        }
        if let Some(index) = self.shell.document_index(id) {
            self.shell.activate_document(index);
        }
        self.save_if_changed();
    }

    /// Closes the open document at `index` and saves state.
    pub fn close_document(&mut self, id: DocumentId) {
        if self.shutting_down.get() {
            return;
        }
        if let Some(index) = self.shell.document_index(id) {
            self.shell.close_document(index);
        }
        self.save_if_changed();
    }

    /// Resolves a discard question without applying consent to newer edits or another tab.
    pub fn answer_discard(&mut self, request: crate::shell::DiscardRequest, discard: bool) {
        if self.shutting_down.get() {
            return;
        }
        self.shell.answer_discard(request, discard);
        if let Err(error) = self.drain_requests() {
            self.report_failure("Document action failed", &error);
        }
        self.save_if_changed();
    }

    /// Runs completions posted to the inbox by background threads. Returns
    /// how many ran. Called by the front end when the inbox waker fires.
    pub fn pump(&mut self) -> usize {
        let started = Instant::now();
        let mut count = 0;
        while count < 32 && started.elapsed() < Duration::from_millis(4) {
            let Some(completion) = self.services.reader().pop() else {
                break;
            };
            count += 1;
            (completion.apply)(&mut CommandContext {
                shell: &mut self.shell,
                services: self.services.access(completion.owner.as_ref()),
                args: None,
            });
            if let Err(error) = self.drain_requests() {
                self.report_failure("Background task failed", &error);
            }
        }
        if count > 0 {
            self.save_if_changed();
        }
        self.services.reader().wake_pending();
        count
    }

    /// The state that would be written by a save right now.
    pub fn persisted_state(&self) -> PersistedState {
        PersistedState {
            schema: STATE_SCHEMA_VERSION,
            documents: self
                .shell
                .documents()
                .iter()
                .map(|document| PersistedDocument {
                    kind: document.kind.to_string(),
                    resource: document.resource.clone(),
                    state: document.provider.persisted_state(),
                })
                .collect(),
            active_document: self.shell.active_document_index(),
            active_views: [ViewLocation::Left, ViewLocation::Bottom]
                .into_iter()
                .filter_map(|location| {
                    self.shell
                        .active_view(location)
                        .map(|view| (location.as_str().to_owned(), view.to_string()))
                })
                .collect(),
        }
    }

    /// Installs the front-end wake callback and signals already queued work.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        self.services.reader().set_waker(waker);
    }

    /// Waits for queued work in a front end without an event loop.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.services.reader().wait(timeout)
    }

    /// A handle for explicit background flushes, including application shutdown.
    pub fn persistence(&self) -> Option<crate::Persistence> {
        self.persistence.clone()
    }

    /// Closes the completion inbox and returns the handle for a final state flush.
    /// The front end must stop accepting input before calling this.
    pub fn begin_shutdown(&self) -> Option<crate::Persistence> {
        if !self.shutting_down.replace(true) {
            self.services.reader().close();
            self.services.shutdown();
        }
        self.persistence()
    }

    fn save_if_changed(&mut self) {
        let Some(writer) = &self.persistence else {
            return;
        };
        let state = self.persisted_state();
        writer.submit(state);
        let error = writer.error();
        if error != self.last_save_error {
            if let Some(error) = &error {
                let message = format!("Could not save state: {error}");
                self.shell.trace_mut().record("state.save_failed", &message);
                self.shell.output().append(message, Tone::Error);
            }
            self.last_save_error = error;
        }
    }

    fn restore_state(&mut self) {
        let Some(store) = &mut self.store else { return };
        let path = store.path().display().to_string();
        let state = match store.load() {
            Ok(LoadOutcome::Fresh) => return,
            Ok(LoadOutcome::Loaded(state)) => state,
            Ok(LoadOutcome::Quarantined { moved_to, reason }) => {
                self.shell.output().append(
                    format!(
                        "Saved state at {path} could not be read ({reason}). It was kept as {} and a fresh session was started.",
                        moved_to.display()
                    ),
                    Tone::Warning,
                );
                self.shell.trace_mut().record("state.quarantined", reason);
                return;
            }
            Ok(LoadOutcome::NewerSchema { found }) => {
                self.shell.output().append(
                    format!(
                        "Saved state at {path} was written by a newer version (schema {found}). It was left untouched and this session will not be saved."
                    ),
                    Tone::Warning,
                );
                self.shell
                    .trace_mut()
                    .record("state.newer_schema", found.to_string());
                return;
            }
            Err(error) => {
                self.shell.output().append(
                    format!("Could not read saved state at {path}: {error}"),
                    Tone::Error,
                );
                return;
            }
        };

        let mut restored = 0;
        let mut active = None;
        for (position, document) in state.documents.iter().enumerate() {
            let opened = DocumentKind::new(&document.kind)
                .map_err(|error| CommandError::Failed(error.to_string()))
                .and_then(|kind| self.open_document(&kind, &document.resource));
            match opened {
                Ok(index) => {
                    if let Some(saved) = &document.state {
                        self.shell.documents()[index].provider.restore_state(saved);
                    }
                    if state.active_document == Some(position) {
                        active = Some(index);
                    }
                    restored += 1;
                }
                Err(error) => {
                    self.shell.requests.clear();
                    self.shell.output().append(
                        format!(
                            "Could not reopen {} `{}`: {error}",
                            document.kind, document.resource
                        ),
                        Tone::Warning,
                    );
                }
            }
        }
        if let Some(index) = active {
            self.shell.activate_document(index);
        }
        for (location, view) in &state.active_views {
            let (Some(location), Ok(view)) = (ViewLocation::parse(location), ViewId::new(view))
            else {
                continue;
            };
            if self
                .contributions
                .view(&view)
                .is_some_and(|spec| spec.location == location)
            {
                self.shell.set_active_view(location, view);
            }
        }
        self.shell
            .trace_mut()
            .record("state.restored", format!("{restored} documents"));
        if restored > 0 {
            self.shell
                .set_status(format!("Restored {restored} documents"));
        }
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        self.begin_shutdown();
        if let Some(writer) = &self.persistence
            && let Err(error) = writer.shutdown(Duration::from_secs(2))
        {
            eprintln!("State flush failed: {error}");
        }
    }
}
