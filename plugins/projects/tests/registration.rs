//! Provider ownership and activation failure through the public plugin API.
use retrowork_platform::{
    ActivationContext, ActivationError, Dependency, Module, ModuleManifest, ModuleState, Platform,
    task::{Captured, OutputLine, ProcessSpec},
};
use retrowork_projects::{
    ActionId, Interpretation, Project, ProjectProvider, ProjectRegistry, ProjectsModule, Scope,
};
use semver::Version;
use std::{path::Path, sync::Arc};

struct Provider(&'static str);
impl ProjectProvider for Provider {
    fn actions(&self) -> &'static [retrowork_projects::ActionDefinition] {
        retrowork_projects::STANDARD_ACTIONS
    }
    fn id(&self) -> &'static str {
        self.0
    }
    fn name(&self) -> &'static str {
        self.0
    }
    fn detect(&self, _: &Path) -> bool {
        true
    }
    fn discovery(&self, _: &Path) -> ProcessSpec {
        panic!("ambiguous providers must not run")
    }
    fn parse_discovery(&self, _: &Path, _: &Captured) -> Result<Project, String> {
        unreachable!()
    }
    fn availability(&self, _: &Project, _: &ActionId, _: &Scope) -> Result<(), String> {
        Ok(())
    }
    fn plan(
        &self,
        _: &Project,
        _: &ActionId,
        _: &Scope,
        _: &serde_json::Value,
    ) -> Result<ProcessSpec, String> {
        unreachable!()
    }
    fn interpret(&self, _: &Project, _: &OutputLine) -> Vec<Interpretation> {
        unreachable!()
    }
}
struct Contributor {
    id: &'static str,
    provider: &'static str,
    fail: bool,
    twice: bool,
}
impl Module for Contributor {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest::bundled(self.id, self.id, Version::new(1, 0, 0))
            .depends_on(Dependency::literal(retrowork_projects::MODULE_ID, "^0.1"))
    }
    fn activate(&self, cx: &mut ActivationContext<'_>) -> Result<(), ActivationError> {
        let registry = cx.require_service::<ProjectRegistry>()?;
        registry.register(cx, Arc::new(Provider(self.provider)))?;
        if self.twice {
            registry.register(cx, Arc::new(Provider(self.provider)))?;
        }
        if self.fail {
            return Err(ActivationError::new("fail after registration"));
        }
        Ok(())
    }
}
fn contributor(id: &'static str, provider: &'static str) -> Contributor {
    Contributor {
        id,
        provider,
        fail: false,
        twice: false,
    }
}
fn active(platform: &Platform, id: &str) -> bool {
    platform
        .modules()
        .iter()
        .find(|report| report.manifest.id.as_str() == id)
        .is_some_and(|report| report.state == ModuleState::Active)
}
#[test]
fn failed_and_duplicate_contributions_leave_no_provider_behind() {
    for twice in [false, true] {
        let mut rejected = contributor("test.a", "cargo");
        rejected.fail = !twice;
        rejected.twice = twice;
        let platform = Platform::builder()
            .module(ProjectsModule)
            .module(rejected)
            .module(contributor("test.b", "cargo"))
            .module(contributor("test.c", "cargo"))
            .build();
        assert!(!active(&platform, "test.a"));
        assert!(active(&platform, "test.b"));
        assert!(!active(&platform, "test.c"));
    }
}
#[test]
fn competing_providers_report_a_conflict_without_running_either_tool() {
    let mut platform = Platform::builder()
        .module(ProjectsModule)
        .module(contributor("test.a", "zeta"))
        .module(contributor("test.b", "alpha"))
        .build();
    let directory = tempfile::tempdir().unwrap();
    platform
        .execute(
            &retrowork_platform::CommandId::literal(retrowork_projects::COMMAND_IMPORT),
            Some(&serde_json::json!({"directory": directory.path()})),
        )
        .unwrap();
    let workspace = platform
        .services()
        .get::<retrowork_projects::ProjectWorkspace>()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while workspace.is_discovering() {
        assert!(std::time::Instant::now() < deadline);
        platform.wait(std::time::Duration::from_millis(10));
        platform.pump();
    }
    assert!(
        platform
            .shell()
            .status()
            .contains("matching project provider")
    );
    assert!(workspace.project().is_none());
}
