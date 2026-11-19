# Testing

Performance testing is suspended until exclusive host access is available. `bash scripts/check.sh` runs all other gates; `cargo run --release -p retrowork-testkit --bin retrowork-perf` remains available for explicitly requested exclusive-host runs with unchanged budgets.

## Tiers

Each tier says exactly what reaches the application. Do not describe one as
another.

| Tier | Where | Input enters at | What it proves |
|---|---|---|---|
| Unit and contract tests | `crates/platform`, `plugins/*` | function calls | Module resolution, activation, commands, persistence, packages, parsing, filtering, statistics. |
| Process-plugin tests | `crates/platform/tests/process_plugin.rs` | real child processes | Deadlines, overload, floods, crashes, shutdown cleanup against misbehaving shell-script plugins. |
| Task-runner tests | `crates/platform/tests/task.rs` | real child processes | Streaming, line limits, backpressure, process-group cancellation, `SIGKILL` escalation also after the leader exited, leftover descendants killed at task end, drop after the leader exited, capture size limits and one deadline when a child or a detached process holds the pipes. |
| Projects contract tests | `plugins/projects/tests/projects.rs` | platform API, shell-script provider | Opening, failures, scope, task states, bounded output, diagnostics to source lines, cancellation. |
| Cargo tests | `plugins/cargo/tests/cargo.rs` | platform API, real `cargo` | Discovery, build, run, test and diagnostics on copies of `fixtures/projects`, offline. |
| Ninja tests | `plugins/ninja/tests/ninja.rs` | platform API, real `ninja` and C compiler | Discovery, shared actions, custom Clean, missing targets and invalid options. |
| Headless GPUI | `crates/testkit/tests/headless.rs` | GPUI test platform events | The real `Workbench` with real layout, hit testing, focus and key dispatch. Nothing is drawn to a screen. |
| Native E2E | `retrowork-e2e` (`cargo run -p retrowork-testkit --bin retrowork-e2e`) | X server events from XTEST | The demo executable in a real window on a private X server: platform input layer, rendering, process plugin, Cargo/Ninja projects, persistence across restart. |
| Injected E2E | `retrowork-e2e --input injected` | GPUI event dispatch inside the running app | Same scenarios without the X input layer. Diagnostic only; not part of the gate. |
| Performance | `retrowork-perf` (`cargo run --release -p retrowork-testkit --bin retrowork-perf`) | XTEST and direct calls | Release build against budgets; see [quality.md](quality.md). |

## Scenarios

The source-editing workflow types into the actual editor, rejects dirty tab
closure and Exit, saves through Ctrl+S and File → Save, checks undo/redo across
Save, then closes and reopens the file to verify its disk contents. Headless
interaction tests also cover initially empty documents. The `doc.save-state`
probe exposes Saved, Modified, Saving and save errors alongside screenshots.
Filesystem tests use the installed `setfacl` command to verify preservation of
access ACLs and removal of unwanted inherited ACLs during replacement.

Discard confirmation uses the component library's actual modal and buttons.
Headless interaction tests cover Escape, pointer cancellation, blocked input
behind the modal, explicit discard and cancelling a multi-document quit.
The native editing workflow cancels close/quit, then discards through the
button and reopens the file to verify that disk contents were unchanged.
Contract tests reject consent for newer revisions, saves in progress and
already closed documents. Modal checkpoints paint the existing automation
frame marker above the backdrop so its shading cannot hide the evidence.

Save As tests cover no-clobber publication, unchanged originals, deferred
session completion, edits arriving during a save, resource reservations,
failed/stale results and persisted destination changes. The headless picker
test validates cancellation and document-bound responses using GPUI's test
platform; it does not claim to operate an OS dialog. The native editing
workflow uses XTEST in the actual system Save As dialog, verifies the new tab
name and undo/redo, saves again, then reopens the unchanged original.

Save All tests cover multiple projects, inactive source tabs, concurrent Save As,
independent disk-conflict failures, and retrying a captured document while another
tab is active. Headless controls exercise both its shortcut and menu item; the
native editing workflow edits two files, saves them together and reopens both
from disk. Write errors must retain edits and identify the file in Output.

Reload tests reject stale consent, newer edits during reads, concurrent writes,
changed file targets and read-only replacements. Integration tests cover missing
files, retry and saving against the reloaded baseline. Headless UI tests operate
the menu and discard controls while the fixture changes on disk. The native
editing workflow cancels and confirms Reload from Disk and checks that Undo
cannot restore the discarded buffer.

The disposable file-finder experiment has nine correctness/lifecycle tests and
a separate native test using the same XTEST driver. Run the latter with
`cargo test -p retrowork-file-finder-prototype --test native -- --ignored`.
The full gate invokes it explicitly; a normal workspace test run marks it
ignored because it requires a real display. Missing native prerequisites fail
the full gate. Evidence is written to `target/file-finder-e2e`, including
frame-matched screenshots, snapshots, traces and `report.json`.
The workflow checks matching, pointer/keyboard selection, source opening,
generated-source inclusion, live filesystem updates and closing projects.
Negative controls drop keyboard input, drop clicks and misdirect clicks.
Coverage from the existing six-crate gate does not measure this experiment.

On a shared desktop, prefer an isolated Xvfb display through
`RETROWORK_E2E_DISPLAY`, with its cookie supplied through `XAUTHORITY`.
The file-finder experiment exposed desktop modifier contamination in nested
Xwayland: injected `r` arrived as `super-r`, and screenshot/frame matching also
failed during desktop activity. This is environmental interference, not proof
of a matcher failure. `target/file-finder-desktop-failure` preserves the raw
failure snapshot, trace and explicitly unmatched pixels.
The isolated validation uses Xvfb with Mesa llvmpipe, selected through
`VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json` on this host.
These are real XTEST interactions and software-rendered UI checks, not GPU
performance measurements. Xvfb was unpacked from a signature-verified Fedora
package into `/tmp` for this run; it was not installed system-wide.

User workflows are written once in `crates/testkit/src/scenarios.rs` and run
by both the headless and the native driver. A scenario can only send raw
input (click a position, press a chord, type text) and observe the window
(element snapshot, trace). It cannot call commands or handlers, so it passes
only if input travels the real route.

Scenarios find controls by semantic id and click the centre of their visible
part. They wait for observable conditions with a deadline, never for a fixed
time. Ids:

| Id | Element |
|---|---|
| `menu.<menu>`, `menu.<menu>.<item>` | menu title, item of the open menu |
| `toolbar.<item>` | toolbar button |
| `dock.<left\|bottom>.tab.<view>` | docked view tab |
| `view.<view>.node.<node>` | tree node; value `expanded`, `collapsed` or `leaf` |
| `view.<view>.line.<n>`, `view.<view>.prop.<name>` | text line, property row |
| `doc.tab.<kind>:<resource>`, `….close` | document tab and its close button |
| `doc.filter`, `doc.summary`, `doc.row.<n>` | active document's filter field, summary, rows |
| `doc.empty`, `view.<view>.empty` | placeholder text |
| `status.message`, `status.modules` | status bar |

The headless restart test also uses the real filesystem writer. It explicitly
permits external I/O wakeups in GPUI's test scheduler and completes the first
instance's shutdown before starting another against that state directory.
Persistence submission/coalescing/deadline tests use a controllable writer;
they are contract tests, not UI interaction or performance measurements.
Native restart workflows exercise the production worker wakeup and quit paths.
A headless flood test fills the completion queue, operates pointer and
keyboard controls while work remains, and then checks that all work drains.
Headless frames explicitly deliver GPUI next-frame callbacks. Key presses
include release events, so GPUI's keyboard-to-click handling is exercised.
Both drivers test counter focus, Enter and Space; observation tests reject missing
and duplicate declared controls.

## Projects in UI tests

Project scenarios run only natively, because they need real processes and
time. Each application instance starts in its own evidence directory, with
`fixtures/projects/` copied to `fixtures/`; scenarios type `fixtures/workspace`
or `fixtures/broken` into the project field. Cargo runs offline, and the
runner removes variables that would leak the test environment into it
(`RUSTFLAGS`, `CARGO_TARGET_DIR`, `LLVM_PROFILE_FILE` and similar). Steps that
wait for Cargo may take up to three minutes; all other waits keep the
driver's patience. The fixtures have no dependencies; tests never open a
project outside them. The Ninja scenario opens `fixtures/ninja`, then uses
Build, Run, Test and the contributed Clean menu item through real input.

## Negative controls

Both drivers run scenarios through `Sabotage`, which drops pointer input,
misaims it by 40 px, or drops keyboard input; each run must fail. The native
runner also drops keyboard input while opening a project. A fifth control
captures old counter pixels, changes the counter through actual
input, and requires the frame check to reject those old pixels before saving
a matching new screenshot. These five native negative controls must all be
detected. The headless suite also replaces Log Statistics with a stand-in
whose handler does nothing or whose shortcut runs another command, and removes a package's
menu items; the affected scenarios must fail at the expected step.

## Control coverage

The native runner asks the application for every contributed control (menu
items, toolbar buttons, dock tabs, shortcuts) and counts those a passing
scenario operated, from `ui.activate` events. Controls operated just before
the application quits are read from its exit report. A control no scenario
operates fails the run. A new contributed control therefore needs a scenario
step with an assertion on what it visibly does.

## Evidence

`target/e2e/` after a native run:

- `report.json`: tier, input route, display, the GPU adapter the window
  reported, every scenario with its outcome, duration and files, the negative
  controls, and control coverage;
- per scenario: numbered screenshots, each with its matching element snapshot
  and event trace (`*.snapshot.json`, `*.trace.json`), application logs and
  exit reports;
- `xwayland.log` from the private X server.

A screenshot is accepted only when its pixels contain the painted binary marker
of the snapshot requested immediately before capture. A later repaint does not
invalidate that matched image; waiting for two snapshots to agree can starve
capture while an input caret blinks.
The marker includes an inverted row to reject incomplete/mixed markers.
The checkpoint trace ends at the captured frame's observation sequence.
Polling is bounded by a deadline; elapsed time alone never accepts an image.
Unmatched capture timeouts retain explicitly named raw pixels, before/after
snapshots and the trace for diagnosis; these are never accepted checkpoints.
This correlates the X-server image with semantic evidence, not physical
monitor presentation. Ordinary windows do not draw the automation marker.

The line gate measures platform, Projects, Cargo, Ninja, logs and log-stats
with their own tests. It requires at least 90%; workbench, application and
test-kit code are outside that measured scope.

## Running

```sh
cargo test --workspace                               # all cargo tests, including headless GPUI
cargo run -p retrowork-testkit --bin retrowork-e2e                                # native suite with negative controls
cargo run -p retrowork-testkit --bin retrowork-e2e -- --only filter-by-keyboard --skip-negative-controls
cargo run -p retrowork-testkit --bin retrowork-e2e -- --list
cargo test -p retrowork-testkit --test headless filter_by_keyboard
```

The native suite needs a Wayland session with Xwayland, or
`RETROWORK_E2E_DISPLAY` naming an X display with XTEST, plus a Vulkan driver.
Without them it exits with status 2 and says what is missing; the gate fails.
Exit status 1 means a scenario failed, a negative control went undetected, or
a control was not exercised.

## Adding a UI test

1. Give every new control a stable id through the workbench's semantic
   registration (contributed menus, toolbar items, tabs and shortcuts get one
   automatically).
2. Add steps to a scenario, or a new `Scenario`, using only `Ui` operations.
   Assert on what the user would see: element text, selection, focus, Output
   lines, and the trace for cause and order.
3. Add the scenario to `scenario_tests!` in `tests/headless.rs`, unless it
   needs real processes or operating-system behavior.
4. Run `cargo test --workspace` and `cargo run -p retrowork-testkit --bin retrowork-e2e`; look at the screenshots.

## System folder picker

Production folder selection uses GPUI's platform API and the user's desktop portal.
Headless tests inspect the GPUI prompt options and simulate cancellation; they
do not claim to exercise an OS dialog. Native tests start the installed D-Bus,
xdg-desktop-portal and GTK portal backend on the private test display. XTEST types
paths into the real system chooser and confirms or cancels it. No chooser UI or
portal response is implemented by RetroWork. Missing portal prerequisites fail the
gate. The test bus and services are stopped with the test server.

System-picker PNGs capture the chooser's X drawable. They are external-window
evidence, not workbench frame-marker snapshots. Project and editor checkpoints
still require frame-matched workbench screenshots, state and trace. Native tests
wait for the picker to close and its asynchronous result to reach the application
before issuing another command. Filesystem-backed import/open workflows run in
the native suite; GPUI's deterministic headless scheduler does not accept wakes
from those real worker threads.
