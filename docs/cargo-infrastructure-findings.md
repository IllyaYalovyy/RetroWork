# Cargo infrastructure findings

Historical findings from the initial Cargo integration. See [status](status.md) and [refactoring review](refactoring-review.md) for current behavior and remaining requirements.

This pass uses Cargo as a real consumer of the reusable project and plugin APIs. The direction holds: the platform owns activation, commands and task execution; Projects owns the shared project UI and action lifecycle; Cargo supplies discovery, action planning and output interpretation. No Cargo-specific branch was needed in the workbench.

## Fixed in this pass

- **Activation could leak registrations.** A provider previously mutated a shared registry before its module finished activating. An activation error or invalid contribution could leave a rejected plugin's provider active. `ActivationContext::stage_registration` now defers validated mutations until the entire activation succeeds. Project providers use it; tests cover late activation errors, invalid contributions, duplicate staging and undeclared owners.
- **Provider selection depended on registration order.** Duplicate provider identifiers now reject the contributing module. If different providers recognize one directory, opening reports the sorted conflicting identifiers and starts neither tool. A future chooser can resolve genuine polyglot projects explicitly.
- **Discovery admitted overlapping jobs.** Repeated opens now fail while discovery is pending. Thread-start failure resets the state. Discovery remains bounded by its existing timeout and output limit; cancellation and multiple simultaneously open projects remain future work.
- **Target actions trusted the caller.** Cargo now verifies package and target membership against discovery. Build scripts and unrecognized target kinds cannot silently fall back to a whole-package action. Their sources remain browsable, and building their owning package runs Cargo's normal lifecycle.
- **Workspace membership was implicit.** Discovery now uses Cargo's opaque workspace-member identifiers, filters nonmembers, checks completeness and rejects duplicate package names. The generic project model retains package names as action selectors.
- **Application JSON could disappear.** Only known Cargo bookkeeping events are hidden. Application output with an unfamiliar `reason` field is retained.

A full-gate run also exposed a race in the existing process-cleanup test: successful early cleanup could remove the child before the test recorded its kernel identity. The test now releases the parent only after recording the live child, then checks the same cleanup outcome and deadline.

## Real-world test case

The initial offline workspace had two members, an excluded path dependency, a build script, two binaries, a default binary, an example and an integration test. API integration tests discover the correct members, build the workspace, run the default and alternate binaries and example, and run the selected integration test. Native UI automation opens this fixture by typing its path, builds it through the toolbar, selects targets with the mouse, and runs the binary and example with keyboard input. The binary output proves the dependency chain and build script executed.

The foundation cleanup changed project-level Build and Test to honor Cargo default members. Run also leaves Cargo default selection intact; package-level Run honors that package's `default-run`. Explicit whole-workspace actions remain to be added.

## Next architectural pressure points

1. Add provider-declared action capabilities and typed action configuration before adding Debug, Coverage or custom actions. The current Build/Run/Test enum and runtime refusal are adequate for this slice, but not the final cross-tool action contract. Unsupported actions should become visibly unavailable with an explanation.
2. Add immutable project snapshots, refresh and cancellation before manifest watching or multiple open projects. Preserve the current project when a replacement discovery fails. Carry a discovery generation identifier when concurrent discovery is introduced.
3. Add Cargo features, profiles, toolchains, environment and program arguments as validated configuration, with separate argument vectors rather than shell strings. Preserve native Cargo semantics and explain the effective command.
4. Add structured test discovery/results, debugger integration and coverage providers as separate capabilities. Plain Cargo test output is not a structured test explorer.
5. Keep registration commits trusted, synchronous and infallible. The new staging primitive does not sandbox native plugin code or provide unload/rollback after a commit callback panics. Resource ownership and unload tokens need an explicit design before hot reload.

The reusable project API still uses Cargo-shaped package/target examples. A second provider, such as Make or Maven, is needed before claiming the abstraction supports all build tools consistently. The current source view is read-only; this is project infrastructure, not a completed Rust editing experience.

## Validation and performance policy

Performance runs are suspended until exclusive host access is available. The default gate retains formatting, strict Clippy, rustdoc, unit/integration tests, headless interaction tests, native input tests, negative controls and at least 90% coverage in the measured crates. Manual performance tooling and budgets are unchanged. This pass makes no new speed or regression claim.

Local `bash scripts/check.sh` passed on 2026-11-18: 165 unit/integration/headless/source-rule tests, 17 native workflows, 46/46 contributed controls and four detected broken-input controls. Line coverage is 94.26% across platform, Projects, Cargo and the log plugins; this is not whole-application coverage. The new native workflow screenshots were also inspected. Evidence: `target/check.log`, `target/e2e/report.json`, `target/e2e/project-workspace-dependencies/`, and `target/coverage/summary.txt`.

The native screenshots also show long target labels clipped in the fixed-width project dock. Full-label tooltips have since been added; resizable docks are still needed for a polished project browser.

Cargo semantics reference: [metadata](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html) and [build target/package selection](https://doc.rust-lang.org/cargo/commands/cargo-build.html).
