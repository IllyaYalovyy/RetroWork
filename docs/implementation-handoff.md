# Implementation handoff

Current work and remaining requirements: [status](status.md) and [refactoring review](refactoring-review.md). The records below describe earlier passes and must not be treated as current acceptance results.

## Earlier passes

Performance testing is suspended until exclusive host access is available. `bash scripts/check.sh` runs all other gates; `cargo run --release -p retrowork-testkit --bin retrowork-perf` remains available for explicitly requested exclusive-host runs with unchanged budgets.

The Cargo infrastructure pass now passes `bash scripts/check.sh`: 165 tests, 17 native UI workflows, 46/46 contributed controls, four detected broken-input controls, and 94.26% line coverage in platform, Projects, Cargo and log plugins. Performance was excluded. See [Cargo findings](cargo-infrastructure-findings.md) for framework refactorings, test evidence and the next action/configuration API work. No commits, pushes or ktask execution were performed.

Before the Cargo infrastructure pass, accepted the foundation and first Cargo workflow after reviewing the final full gate log, code changes, native UI evidence and performance results. The process-lifecycle findings were fixed with adversarial tests. Final evidence: 160 tests, 16 native UI workflows, 46/46 contributed controls, four broken-input controls, 94.24% line coverage in the measured crates, and 17 performance metrics passing under the previous policy.

One earlier full run failed large-log-open P90 (823.8 ms against 800 ms) under higher host load. The failed evidence was preserved, budgets were unchanged, and the subsequent full run passed. Performance is a coarse local regression baseline; load sensitivity and possible smaller regressions remain limitations, not proof of final product speed.

No further implementation is running. ktask remains configured and idle; no CI/CD, commits or pushes were performed. The one-time scheduled continuation is complete.

# Implementation handoff

## Current state, 2026-11-18 (Opus pass, Cargo project workflow)

The first Cargo project workflow is implemented and the full `bash scripts/check.sh`
passes (exit 0; log `target/check.log`). It awaits Codex review. No commits,
pushes or ktask tasks were made; ktask remains configured and idle.

### What was built

- **Platform, generic:** `task` module that runs tools in their own process
  group and owns it: streamed, bounded, backpressured output; cancellation
  (`SIGTERM`, then `SIGKILL` after 2 s, also after the leader exited);
  leftovers in the group killed at task end; cleanup on drop and on failed
  starts; the leader reaped only after the group is killed; `capture` with
  one deadline for the whole run; view-model additions any module can use: an input field above a
  tree (`InputModel`, `ViewEvent::InputSubmitted`, `FocusTarget::ViewInput`),
  a fixed header line and a selected line on text views. The workbench keeps
  scroll state per list, so views and documents scroll independently.
- **`plugins/projects` (bundled module `retrowork.projects`):** the provider
  contract (`ProjectProvider`, `Project`/`Unit`/`Target`, `StandardAction`,
  `Scope`, `Diagnostic`, `Interpretation`) and the build-tool independent UI:
  Projects tree with a directory field, Build/Run/Test/Stop (Run menu,
  toolbar, F11, F6, Alt+F6, Ctrl+F2, File > Open Project, Ctrl+O), Build
  Output (newest 10,000 lines, state header), Problems, and a read-only
  source document opened at a diagnostic's line. One task at a time.
- **`plugins/cargo` (bundled module `retrowork.cargo`):** the Cargo provider:
  `cargo metadata --no-deps` discovery, scope-narrowed
  `cargo build|run|test --message-format=json`, JSON compiler messages to
  located diagnostics, tones for plain lines.
- **Fixtures:** `fixtures/projects/workspace` (app, greeter with unit and
  integration tests, checks with one failing test, slow ticking program) and
  `fixtures/projects/broken` (one type error). No dependencies; the workspace
  excludes `fixtures`.

### Review fixes in this pass

- The task runner owns the process group for the task's whole life: leftover
  processes are killed when the task ends, cancel escalation to `SIGKILL`
  also applies after the leader exited, dropping the handle after the leader
  exited kills what is left, and the leader is reaped only after its group is
  killed, so no signal can reach a reused id. `capture` has one deadline
  across the exit and both pipes. Failed starts (thread creation) kill and
  reap the group through the group's drop; that path itself is not
  injected in a test.
- The workbench's empty-document hint is generic: "No document is open.
  Open one from Sources or Projects.", built from the left views' titles.
- Docs distinguish discovery (`cargo metadata --no-deps`, no project code
  runs) from Build, Run and Test (project code runs).

### Evidence from the final `bash scripts/check.sh`

The first full run after these fixes failed one budget at load 17.2 on 8
CPUs: `large_log_open` P90 823.8 ms > 800 ms (p50 512.7 ms); every other
step passed. Its log and report are kept as `target/check-failed-1.log` and
`target/perf/report-failed-1.json`. No budget was changed; the rerun below,
at load 11.3, passed. Neither the task runner nor the hint is on that
metric's path, but the earlier note about this metric still applies.

- Tests: 160 passed, including 19 task-runner tests and 13 process-plugin
  tests against real processes, 9 Projects contract tests with a
  shell-script provider, 4 tests against real `cargo` on fixture copies, 16
  headless GPUI tests and 7 source-rule tests.
- Native E2E: 16 scenarios passed, among them `project-open-and-browse`,
  `project-build-and-run`, `project-diagnostics`, `project-tests` and
  `project-cancel`, all by XTEST pointer and keyboard input; 46/46
  contributed controls operated; 4 negative controls detected, including
  dropped keyboard input while opening a project. Screenshots in
  `target/e2e/project-*/` were inspected: project tree, source at line,
  Problems row, cancelled run.
- Coverage: 94.24% of 5,624 lines in platform, projects, cargo, logs and
  log-stats (floor 90%): task runner 95.9%, Projects 95.8%, Cargo 97.4%.
  This is not whole-application coverage.
- Performance: 17 metrics within budget (load 11.3 at the start), the
  slow-plugin negative control rejected. `large_log_open` p50 370 ms, P90
  409 ms; `project_discovery_40_units` p50 50 ms; `project_open_to_tree` p50
  183 ms; `task_output_100k_lines` p50 526 ms. All budgets are unchanged
  from the previous pass.
- The `tree-selection-opens-log` start screenshot shows the new hint.

### Limits to review

- `large_log_open` p50 was 484 ms against a 550 ms budget in the gate run,
  and 399–405 ms at load 7–9, above the 204–279 ms of the first baseline.
  The unchanged in-process `log_parse_200k` rose by a similar factor
  (132–236 ms against 107–119 ms), which points to machine conditions, but
  a small regression is not ruled out. Budgets were not raised; under heavy
  load the gate can fail. A run at load 26.5 did fail this metric and
  startup.
- `project_open_to_tree` (≈ 240–420 ms) is well above discovery alone
  (≈ 55–110 ms); the difference was not investigated.
- Cargo projects with dependencies, persistence of the opened project,
  per-test results, editing and file watching are not done. Opening a
  project runs only `cargo metadata --no-deps`, which executes no project
  code; Build, Run and Test execute project code (build scripts, procedural
  macros, the program, its tests). The README says so.
- Tools that leave their process group (`setsid`, `setpgid`) survive Stop
  and task end; if one holds a pipe, its reader thread lives until it exits.
  The task runner is Unix only.
- Project scenarios run natively only; headless GPUI tests do not cover
  them, because they need real processes and time.
- A full `bash scripts/check.sh` includes a release build and can exceed ktask's
  registered 1,800,000 ms timeout on a cold build cache.

### Next

Proposed in `docs/status.md`: restore the opened project after restart,
test results as a tree, a second provider to prove the contract, an editable
source document with save and recovery. Keep ktask idle until review.

Every future commit must have Illya Yalovyy
<1623643+IllyaYalovyy@users.noreply.github.com> as sole author, follow the
commit-message rules in AGENTS.md, and never bypass hooks. The project stays
Apache-2.0.

## History

- Fable pass 1 (2026-11-16): platform, plugins, and a spike proving GPUI,
  XTEST input and screenshots on nested Xwayland; stopped at its usage limit.
- Fable pass 2 (2026-11-17): workbench, demo, testkit, headless and native
  E2E, process-host review fixes with adversarial tests; stopped with the
  gate failing at 27/29 control coverage.
- Opus pass 1 (2026-11-17): exit-control coverage through exit reports
  (29/29), required 90% line coverage, the performance gate with a slow-plugin
  negative control, the line-based documentation rule, developer
  documentation; full gate passed and was accepted provisionally.
- Opus pass 2 (2026-11-17): parser-based documentation rule, semantic-state
  wording for window timings, renderer recorded from the window, padding
  review, `target/check.log`; accepted by Codex.
- Codex installed llvm-tools-preview and cargo-llvm-cov 0.9.1. The user
  installed libxkbcommon-x11-devel, which removed the temporary linker shim.
- Codex direction for this pass: fix exit-control coverage, require line
  coverage, add a mandatory performance gate with negative controls, enforce
  code clarity, finish local documentation without CI/CD, keep ktask idle,
  no commits.
- ktask project configuration (not versioned here): Sonnet execution; Opus
  planning, review, validation and remediation; all high effort; registered
  gate `bash scripts/check.sh`.
