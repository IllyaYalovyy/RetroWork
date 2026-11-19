//! Project detection, import and opening independent sessions.

use super::*;

/// Validated project data ready to become a workspace session.
struct Discovered {
    project: Project,
    provider: Option<Arc<dyn ProjectProvider>>,
    navigation: navigation::Navigation,
}

/// Resolves what the user typed: `~` is the home directory and relative paths
/// start at the application's working directory.
pub(super) fn resolve_directory(text: &str) -> Result<PathBuf, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("type the directory of a project".to_owned());
    }
    let expanded = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = std::env::var_os("HOME").ok_or("HOME is not set, so `~` is unknown")?;
            PathBuf::from(home).join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(text),
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .map_err(|error| format!("cannot read the working directory: {error}"))?
            .join(expanded)
    };
    if !absolute.is_dir() {
        return Err(format!("{} is not a directory", absolute.display()));
    }
    absolute
        .canonicalize()
        .map_err(|error| format!("cannot open {}: {error}", absolute.display()))
}

impl ProjectWorkspace {
    /// Starts opening the directory the user typed. Discovery runs on a
    /// worker thread; its result arrives through the inbox.
    pub(super) fn open(
        &self,
        text: &str,
        import: bool,
        cx: &mut CommandContext<'_>,
    ) -> Result<(), CommandError> {
        if self.is_discovering() {
            return Err(CommandError::failed("project discovery is already running"));
        }
        if text.trim().is_empty() {
            return Err(CommandError::failed("type the directory of a project"));
        }
        let directory = PathBuf::from(text.trim());
        let providers = self.registry.providers();
        let cancellation = task::Cancellation::default();
        let publication = Arc::new(AtomicU8::new(0));
        let generation = {
            let mut state = self.state.borrow_mut();
            state.next_discovery += 1;
            let generation = state.next_discovery;
            state.open_error = None;
            state.pending = Some(Discovery {
                generation,
                cancellation: cancellation.clone(),
                publication: publication.clone(),
            });
            generation
        };
        cx.shell
            .set_status(format!("Opening project {}…", directory.display()));
        cx.shell
            .trace_mut()
            .record("project.discovering", directory.display().to_string());

        let inbox = cx.services.inbox();
        let spawned = std::thread::Builder::new()
            .name("project-discovery".into())
            .spawn(move || {
                let result = (|| {
                    let directory = resolve_directory(&directory.to_string_lossy())?;
                    let configured = if import {
                        if directory.join(config::CONFIG_PATH).exists() {
                            return Err("project is already configured; use Open Project".into());
                        }
                        None
                    } else {
                        Some(config::ProjectConfig::load(&directory)?)
                    };
                    let matches: Vec<_> = providers
                        .into_iter()
                        .filter(|provider| match &configured {
                            Some(config) => config.provider == provider.id(),
                            None => provider.detect(&directory),
                        })
                        .collect();
                    let plain = configured.as_ref().map_or(matches.is_empty(), |config| {
                        config.provider == "projects.files"
                    });
                    let (mut project, provider) = if plain {
                        (
                            Project {
                                provider: "projects.files".into(),
                                name: directory
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into(),
                                root: directory.clone(),
                                units: Vec::new(),
                            },
                            None,
                        )
                    } else {
                        if matches.len() != 1 {
                            return Err(
                                "the folder needs exactly one installed matching project provider"
                                    .into(),
                            );
                        }
                        let provider = matches.into_iter().next().unwrap();
                        let captured = task::capture_with_cancel(
                            &provider.discovery(&directory),
                            MAX_DISCOVERY_BYTES,
                            DISCOVERY_TIMEOUT,
                            &cancellation,
                        )?;
                        let project = provider.parse_discovery(&directory, &captured)?;
                        project.validate(provider.id())?;
                        if project.root != directory {
                            return Err(format!(
                                "select the build workspace root: {}",
                                project.root.display()
                            ));
                        }
                        (project, Some(provider))
                    };
                    let config = configured.unwrap_or_else(|| {
                        config::ProjectConfig::new(
                            project.name.clone(),
                            project.provider.clone(),
                            Vec::new(),
                        )
                    });
                    if cancellation.is_cancelled() {
                        return Err("project discovery cancelled".into());
                    }
                    if import {
                        publication
                            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                            .map_err(|_| "project discovery cancelled".to_owned())?;
                        config.create(&directory)?;
                    }
                    project.name = config.name.clone();
                    let suggested = provider.as_ref().map_or_else(
                        || config::conventional_roots(&project),
                        |provider| provider.logical_roots(&project),
                    );
                    let navigation = navigation::Navigation::new(&project, &config, suggested);
                    Ok(Discovered {
                        project,
                        provider,
                        navigation,
                    })
                })();
                let _ = inbox.post_blocking(Box::new(move |cx| {
                    if let Some(workspace) = cx.services.get::<ProjectWorkspace>() {
                        workspace.discovered(generation, directory, result, cx);
                    }
                }));
            });
        match spawned {
            Ok(worker) => {
                drop(worker);
                Ok(())
            }
            Err(error) => {
                let mut state = self.state.borrow_mut();
                state.pending = None;
                Err(CommandError::Failed(format!(
                    "cannot start discovery: {error}"
                )))
            }
        }
    }

    fn discovered(
        &self,
        generation: u64,
        directory: PathBuf,
        result: Result<Discovered, String>,
        cx: &mut CommandContext<'_>,
    ) {
        let mut state = self.state.borrow_mut();
        if !state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.generation == generation)
        {
            return;
        }
        let cancelled = state.pending.as_ref().unwrap().cancellation.is_cancelled();
        state.pending = None;
        if cancelled {
            cx.shell.set_status("Project discovery cancelled");
            cx.shell
                .trace_mut()
                .record("project.cancelled", directory.display().to_string());
            return;
        }
        match result {
            Ok(Discovered {
                project,
                provider,
                navigation,
            }) => {
                let existing = std::iter::once(&state.active)
                    .chain(state.others.values())
                    .find(|session| {
                        session
                            .current
                            .as_ref()
                            .is_some_and(|current| current.project.root == project.root)
                    })
                    .map(|session| session.id);
                if let Some(id) = existing {
                    state.activate(id).expect("the existing session was found");
                    cx.shell.set_status("Project is already open");
                    return;
                }
                let message = format!(
                    "Opened {} ({} {}, {})",
                    project.name,
                    project.units.len(),
                    if project.units.len() == 1 {
                        "unit"
                    } else {
                        "units"
                    },
                    provider
                        .as_ref()
                        .map_or("Files", |provider| provider.name())
                );
                cx.shell.trace_mut().record("project.opened", &project.name);
                cx.shell.set_status(message);
                state.next_id += 1;
                let id = state.next_id;
                let next = Session {
                    navigation,
                    id,
                    current: Some(ProjectSession {
                        project: Arc::new(project),
                        provider,
                    }),
                    selected: Some(format!("p{id}/project")),
                    ..Session::default()
                };
                let previous = std::mem::replace(&mut state.active, next);
                if previous.current.is_some() {
                    state.others.insert(previous.id, previous);
                }
            }
            Err(reason) => {
                let message = format!("Cannot open {}: {reason}", directory.display());
                cx.shell.trace_mut().record("project.failed", &message);
                cx.shell.output().append(&message, Tone::Error);
                cx.shell.set_status(message.clone());
                state.open_error = Some(message);
            }
        }
    }
}
