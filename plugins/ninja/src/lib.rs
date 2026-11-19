//! Ninja project support through the shared project and task contracts.

use retrowork_platform::{
    ActivationContext, ActivationError, Dependency, Module, ModuleManifest, Tone,
    task::{Captured, OutputLine, ProcessSpec, TaskOutcome},
};
use retrowork_projects::{
    ActionDefinition, ActionId, Interpretation, Project, ProjectProvider, ProjectRegistry, Scope,
    Target, TargetKind, Unit,
};
use std::{path::Path, sync::Arc};

/// Reads Ninja's own target listing and delegates actions to Ninja.
pub struct NinjaProvider;
impl ProjectProvider for NinjaProvider {
    fn id(&self) -> &'static str {
        "ninja"
    }
    fn name(&self) -> &'static str {
        "Ninja"
    }
    fn actions(&self) -> &'static [ActionDefinition] {
        &[
            ActionDefinition {
                id: "build",
                title: "Build",
            },
            ActionDefinition {
                id: "run",
                title: "Run",
            },
            ActionDefinition {
                id: "test",
                title: "Test",
            },
            ActionDefinition {
                id: "ninja.clean",
                title: "Clean Ninja Outputs",
            },
        ]
    }
    fn detect(&self, directory: &Path) -> bool {
        directory.join("build.ninja").is_file()
    }
    fn discovery(&self, directory: &Path) -> ProcessSpec {
        ProcessSpec::new("ninja", directory).args(["-t", "targets", "all"])
    }
    fn parse_discovery(&self, directory: &Path, output: &Captured) -> Result<Project, String> {
        if output.outcome != TaskOutcome::Exited(0) {
            return Err(format!("Ninja discovery failed: {}", output.stderr.trim()));
        }
        let mut targets = Vec::new();
        let mut ids = std::collections::BTreeSet::new();
        for line in output.stdout.lines() {
            let (id, rule) = line
                .rsplit_once(": ")
                .ok_or("invalid Ninja target listing")?;
            if id.is_empty() || !ids.insert(id.to_owned()) {
                return Err("duplicate or empty Ninja target".into());
            }
            targets.push(Target {
                id: id.into(),
                name: id.into(),
                kind: TargetKind::Other(rule.into()),
                source: None,
            });
        }
        Ok(Project {
            provider: "ninja".into(),
            name: directory
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            root: directory.into(),
            units: vec![Unit {
                id: "build".into(),
                name: "build.ninja".into(),
                version: None,
                manifest: directory.join("build.ninja"),
                targets,
            }],
        })
    }
    fn availability(
        &self,
        project: &Project,
        action: &ActionId,
        scope: &Scope,
    ) -> Result<(), String> {
        if !self.actions().iter().any(|item| item.id == action.as_str()) {
            return Err("unsupported Ninja action".into());
        }
        let unit = match scope {
            Scope::Project => "build",
            Scope::Unit(unit) | Scope::Target { unit, .. } => unit,
        };
        let unit = project
            .unit(unit)
            .ok_or("Ninja build unit no longer exists")?;
        if let Scope::Target { target, .. } = scope {
            if !unit.targets.iter().any(|item| &item.id == target) {
                return Err("Ninja target no longer exists".into());
            }
            if *action != ActionId::BUILD {
                return Err("select the project for Run, Test or Clean".into());
            }
        } else if matches!(action.as_str(), "run" | "test")
            && !unit
                .targets
                .iter()
                .any(|target| target.id == action.as_str())
        {
            return Err(format!(
                "build.ninja does not declare a `{}` target",
                action.as_str()
            ));
        }
        Ok(())
    }
    fn plan(
        &self,
        project: &Project,
        action: &ActionId,
        scope: &Scope,
        options: &serde_json::Value,
    ) -> Result<ProcessSpec, String> {
        self.availability(project, action, scope)?;
        if !options.is_null() && options.as_object().is_none_or(|object| !object.is_empty()) {
            return Err("Ninja actions accept no options yet".into());
        }
        let mut args = Vec::new();
        match action.as_str() {
            "ninja.clean" => args.extend(["-t".to_owned(), "clean".to_owned()]),
            "build" => {
                if let Scope::Target { target, .. } = scope {
                    args.extend(["--".to_owned(), target.clone()]);
                }
            }
            other => args.push(other.to_owned()),
        }
        Ok(ProcessSpec::new("ninja", &project.root).args(args))
    }
    fn interpret(&self, _: &Project, line: &OutputLine) -> Vec<Interpretation> {
        vec![Interpretation::Line {
            text: line.text.clone(),
            tone: Tone::Normal,
        }]
    }
}

/// Registers Ninja and its custom Clean action with the project platform.
pub struct NinjaModule;
impl Module for NinjaModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled("retrowork.ninja", "Ninja", semver::Version::new(0, 1, 0))
            .depends_on(Dependency::literal(retrowork_projects::MODULE_ID, "^0.1"))
    }
    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        cx.require_service::<ProjectRegistry>()?
            .register(cx, Arc::new(NinjaProvider))
    }
}
