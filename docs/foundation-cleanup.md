# Foundation cleanup

This records the earlier cleanup pass. See [status](status.md) for current validation and [refactoring review](refactoring-review.md) for the subsequent contract changes and remaining work.

## Implemented

1. Removed Make and its Cargo aliases. The application is the workspace default member. Direct Cargo commands remain direct; the existing shell check script orchestrates only the multi-step acceptance run and retains its log.
2. Removed implicit package discovery from the current directory. Ordinary startup has no sample-plugin executable requirement. Explicit `--packages` or `RETROWORK_PACKAGES` selects packages. Only `--samples` enables demo modules; installing ordinary packages does not enable them.
3. Removed hardcoded build-artifact paths from the check script. Cargo locates the E2E runner; that runner finds its companion executables beside itself. The application uses the same adjacency rule for the optional process sample.
4. Bounded the shared completion queue and each UI pump. Wake notifications are coalesced and bounded. Shutdown closes the inbox before releasing services and unblocks waiting producers. Regression tests exercise full queues, UI-thread rejection, shutdown, and turn limits. These are functional tests, not new performance measurements.
5. Added command availability checks owned by the contributing module. Build/Run/Test controls expose reasons through semantic state and tooltips, and dispatch enforces the same predicate. Native tests check unavailable actions before opening a project and after selecting a library, then availability after selecting a binary.
6. Added plugin-owned view models and native renderer registration through the workbench module. Registration commits only after successful activation. Plugins can render GPUI content and route custom events to their provider. The optional native counter sample and a real-click workflow exercise the public contract.
7. Removed implicit `--workspace` from ordinary Cargo Build/Test. A default-member fixture includes a deliberately failing non-default member: ordinary Build succeeds, explicitly building that member fails.
8. Preserved the prior project state and selection when replacement discovery fails. Added a regression test.
9. Added full-label tooltips to project tree rows and reconciled current documentation. Historical test evidence is not evidence that the current architecture is complete.

## Deliberate choices after review

- Keep the pinned GPUI Kit stack for now: the workbench uses its GPUI re-exports, input and tooltip components. Replacing it with raw GPUI would require replacing those components. This pass does not establish long-term upstream suitability; dependency upgrades remain coupled and must run the local gate.
- Keep manual Cargo JSON parsing in this pass. It is a small, tested adapter but still a maintenance risk. Compare a typed Cargo metadata/message library before expanding discovery or adding more compiler-message handling. Do not introduce a new parser framework.
- Keep task-group cleanup and native-input tests. They address real failure modes and are independent of the removed Make aliases.

## Follow-up list recorded at that review

This list is historical. The subsequent pass implements action declarations and
configuration, a real Ninja provider, owned resource disposal and correlated
screenshot evidence. See [status](status.md) for current limits.

1. Provider-declared action IDs, capabilities and validated configuration, including custom actions and explicit whole-workspace scope; prove the project abstraction with another build tool.
2. Project refresh, session restoration and multiple-project ownership. The subsequent refactoring added discovery cancellation and kept the current tree visible during replacement; project actions wait for discovery to finish.
3. Explicit plugin resource ownership/disposal and process-request cancellation. Staged callbacks are trusted and must not fail; this is not panic rollback, native-code isolation or hot unload.
4. Resizable docking using the existing UI component library, with real drag tests and saved layout. Editing and recovery remain separate milestones.
5. Broader UI coverage: native Wayland, accessibility, IME and scaling. The renderer extension provides an observation mechanism; it does not automatically make every arbitrary plugin testable. An intermediate native-counter checkpoint captured the preceding painted frame although the semantic snapshot had advanced; its final screenshot showed the expected count. Synchronizing screenshot evidence to presentation remains an explicit test-harness requirement. Do not replace that requirement with arbitrary sleeps or claim prepaint snapshots prove presentation.

## Validation

The earlier cleanup gate passed on 2026-11-18: 169 unit/integration/headless/source-rule tests, 19 native XTEST workflows, 47/47 contributed controls, four detected negative controls, and 93.99% line coverage across platform, Projects, Cargo and log plugins. Formatting, strict Clippy and rustdoc also passed. This is not whole-application coverage. The native plugin's final screenshot was visually inspected and shows the expected updated counter.

The first sandboxed run could not create Xwayland's socket. The complete gate was then rerun with desktop access and passed. Evidence is in `target/check.log`, `target/e2e/report.json`, `target/e2e/native-plugin-view/02-final.png` and `target/coverage/summary.txt`.

After the full check stages passed, the shell logger was adjusted to close and wait for its writer before exiting. The complete fast gate was rerun and exited successfully; native code was unchanged. Plain `cargo run --locked -- --help` also exited successfully.

Performance remained suspended. No commits, pushes or ktask work were performed.
