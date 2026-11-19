//! The second provider exercises discovery, standard actions and a custom action through the platform.

use retrowork_ninja::{NinjaModule, NinjaProvider};
use retrowork_platform::{
    CommandId, Platform, ViewContent, ViewEvent, ViewId, ViewTarget,
    task::{self, Captured, TaskOutcome},
};
use retrowork_projects::{
    ActionId, ProjectProvider, ProjectWorkspace, ProjectsModule, Scope, VIEW_TREE,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("ninja");
    std::fs::create_dir(&root).unwrap();
    for file in ["build.ninja", "main.c"] {
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/projects/ninja")
                .join(file),
            root.join(file),
        )
        .unwrap();
    }
    retrowork_projects::config::ProjectConfig::new("ninja".into(), "ninja".into(), vec![])
        .create(&root)
        .unwrap();
    (directory, root)
}
fn settle(platform: &mut Platform, ready: impl Fn(&Platform) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready(platform) {
        assert!(
            Instant::now() < deadline,
            "project operation did not complete"
        );
        platform.wait(Duration::from_millis(20));
        platform.pump();
    }
}
fn workspace(platform: &Platform) -> std::rc::Rc<ProjectWorkspace> {
    platform.services().get().unwrap()
}

#[test]
fn native_build_run_test_and_custom_clean_share_the_project_lifecycle() {
    let (_directory, root) = fixture();
    let mut platform = Platform::builder()
        .module(ProjectsModule)
        .module(NinjaModule)
        .build();
    assert!(platform.modules().iter().all(|module| module.is_active()));
    platform
        .view_event(
            &ViewTarget::View(ViewId::literal(VIEW_TREE)),
            ViewEvent::InputSubmitted(root.display().to_string()),
        )
        .unwrap();
    settle(&mut platform, |platform| {
        workspace(platform).project().is_some()
    });
    for command in [
        "projects.build",
        "projects.run",
        "projects.test",
        "projects.action.ninja.clean",
    ] {
        platform
            .execute(&CommandId::new(command).unwrap(), None)
            .unwrap();
        settle(&mut platform, |platform| !workspace(platform).is_running());
        let Some(ViewContent::Text(output)) =
            platform.view_content(&ViewId::literal(retrowork_projects::VIEW_OUTPUT))
        else {
            panic!("missing task output");
        };
        assert!(output.header.unwrap().text.contains("succeeded"));
    }
    assert!(!root.join("hello").exists());
    assert!(
        platform
            .execute(
                &CommandId::literal("projects.action.ninja.clean"),
                Some(&serde_json::json!({"bad": true}))
            )
            .is_err()
    );
}

#[test]
fn target_policy_and_invalid_discovery_remain_provider_owned() {
    let (_directory, root) = fixture();
    assert!(NinjaProvider.detect(&root));
    assert!(!NinjaProvider.detect(&root.join("missing")));
    let captured = task::capture(
        &NinjaProvider.discovery(&root),
        16384,
        Duration::from_secs(10),
    )
    .unwrap();
    let project = NinjaProvider.parse_discovery(&root, &captured).unwrap();
    let target = Scope::Target {
        unit: "build".into(),
        target: "hello".into(),
    };
    assert_eq!(
        NinjaProvider
            .plan(&project, &ActionId::BUILD, &target, &serde_json::json!({}))
            .unwrap()
            .args,
        ["--", "hello"]
    );
    assert!(
        NinjaProvider
            .availability(&project, &ActionId::RUN, &target)
            .is_err()
    );
    assert!(
        NinjaProvider
            .availability(
                &project,
                &ActionId::new("unknown").unwrap(),
                &Scope::Project
            )
            .is_err()
    );
    assert!(
        NinjaProvider
            .availability(&project, &ActionId::BUILD, &Scope::Unit("gone".into()))
            .is_err()
    );
    assert!(
        NinjaProvider
            .availability(
                &project,
                &ActionId::BUILD,
                &Scope::Target {
                    unit: "build".into(),
                    target: "gone".into()
                }
            )
            .is_err()
    );
    let mut no_run = project.clone();
    no_run.units[0].targets.retain(|target| target.id != "run");
    assert!(
        NinjaProvider
            .availability(&no_run, &ActionId::RUN, &Scope::Project)
            .unwrap_err()
            .contains("does not declare")
    );
    for stdout in ["bad", ": bad", "x: rule\nx: rule"] {
        assert!(
            NinjaProvider
                .parse_discovery(
                    &root,
                    &Captured {
                        stdout: stdout.into(),
                        stderr: String::new(),
                        outcome: TaskOutcome::Exited(0)
                    }
                )
                .is_err()
        );
    }
    assert!(
        NinjaProvider
            .parse_discovery(
                &root,
                &Captured {
                    stdout: String::new(),
                    stderr: "error".into(),
                    outcome: TaskOutcome::Exited(1)
                }
            )
            .is_err()
    );
}
