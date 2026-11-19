# Status

RetroWork is a tested prototype foundation, not a finished IDE or a stable
community-plugin SDK. The reviewed foundation refactorings are implemented.
No commits or pushes were made; ktask remains idle. Performance runs are
suspended until explicitly authorized with exclusive host access.

## Current behavior

- `cargo run` starts the project workbench. Cargo and Ninja are bundled;
  demonstrations require `--samples` and packages require an explicit directory.
  There is no Makefile or task-framework dependency.
- The platform supports transactional contributions, owned services with
  dependency-scoped access, typed registries, reverse-order resource disposal,
  command registrations with availability, and asynchronous persistence/tasks.
- Native views use typed Rust models/events, view-scoped listeners and focus,
  and declared observable controls. Missing or duplicate observations fail tests.
  Tree/table/text/property models remain available. Process plugins cannot add views.
- Documents have non-reused IDs within a platform instance. Closing another tab
  cannot redirect a retained event. Source buffers belong to open documents;
  bounded background reads and diagnostic reopen navigation are implemented.
- Cargo supports discovery, scoped Build/Run/Test/Stop, diagnostics and editable
  source navigation. Command options support release/profile, features, explicit
  workspace Build/Test and program/test arguments; defaults still honor Cargo
  default members. No configuration editor is provided yet.
- Ninja exercises the same project/task infrastructure, including provider-owned
  target IDs, targets without source paths, shared actions and a custom Clean
  action contributed without changing a central action enum.
- Import creates `.retrowork/project.toml`; Open requires it. The system folder
  picker is provided by GPUI and the desktop portal. Multiple projects share a
  window, with independent selections, task output and diagnostics. Logical roots
  come from providers or configuration. Writable files open in the editable GPUI component; read-only files remain viewable.
  Current projects remain visible during discovery; stale results and selections
  cannot redirect project ownership.
- The inbox holds at most 256 completions. Submission is explicitly nonblocking
  or worker-blocking and returns rejected work. Each pump starts at most 32
  callbacks within a 4 ms scheduling window, then yields through GPUI's next-frame
  callback. Individual trusted callbacks cannot be preempted.
- Checkpoints pair pixels, semantic state and trace by decoding an automation-only
  frame marker painted in the same scene. Stale pixels cannot satisfy the check.
  This establishes X-server image correspondence, not physical display timing.

## Evidence

### Reload from Disk

File → Reload from Disk rereads an editable source using the existing bounded
file worker. Dirty files require explicit discard consent; clean files reload
directly. The tab and session identities remain stable. Successful reloads
replace the saved baseline and reset editor selection, scrolling and undo using
the component library's existing reset operation. No new dependency was added.

The reusable discard contract now supports provider events as well as closing.
Its `DiscardRequest` remains bound to one document, revision and single-use
sequence. Cancellation, newer edits, busy work and superseded requests reject
consent. Events run with the owning provider's service access. A worker-loaded
replacement must still match the session's path and edit revision; reads that
fail or arrive after newer edits leave the buffer intact. Completion applies
even when another tab is active.

This is manual recovery, not an external-change watcher or conflict merge UI.
Read-only viewers cannot reload yet, and a file becoming read-only is reported
without replacing the editable buffer. General alias reconciliation remains
outside this increment.

Manual clarity review covers reuse of the modal, editor reset and file worker,
single-use consent, provider-owned delivery, retained state on errors and stale
read rejection. Screenshot review caught a stale cancellation status after a
successful reload; completion now reports the reloaded path or read error, and
the integration, headless and native tests check the success message.

Local acceptance passed on 2026-11-19: formatting, strict Clippy, rustdoc and
246 workspace tests, including 26 headless UI tests. All 22 native workflows,
57 contributed controls and five negative controls passed. The separate
file-finder native workflow and its three negative controls passed. Native tests
used XTEST on isolated Xvfb with llvmpipe rendering. The final reload screenshot
in `target/e2e/source-edit-save-and-close` was reviewed.

Line coverage is 94.09% (8,158 of 8,670 lines) across platform, Projects, Cargo,
Ninja, logs and log-stats; this does not measure the whole application. Evidence
is in `target/check.log`, `target/e2e/report.json`,
`target/file-finder-e2e/report.json` and `target/coverage/summary.txt`.
Performance tests remain suspended. No commits or pushes were made.

### Save All across projects

File → Save All and Ctrl+Alt+S save dirty, editable source documents across open
projects without changing the active tab. The action reuses each document's
save event and the existing bounded worker. It excludes clean, read-only and
loading documents and leaves outstanding writes alone. Later edits to a file
already saving require another save. It is not a multi-file transaction.

Admission failures, including concurrent Save As preparation or a full worker
queue, are reported without preventing attempts on the other documents. Write
failures retain dirty state and appear in Output with their path, even for a
background tab. Save also now uses the same captured document identity for its
availability check and execution.

Manual clarity review covers command routing, bounded queue reuse, independent
failure handling, UI-thread ownership and retained undo history.

Local acceptance passed on 2026-11-18: formatting, strict Clippy, rustdoc and
242 workspace tests, including 25 headless UI tests. All 22 native workflows,
56 contributed controls and five negative controls passed. The separate
file-finder native workflow and its three negative controls also passed. Native
tests used XTEST on isolated Xvfb with llvmpipe rendering. The two-file save and
reopen screenshot was reviewed in `target/e2e/source-edit-save-and-close`.

Line coverage is 93.94% (7,953 of 8,466 lines) across platform, Projects, Cargo,
Ninja, logs and log-stats; this does not measure the whole application. Evidence
is in `target/check.log`, `target/e2e/report.json`,
`target/file-finder-e2e/report.json` and `target/coverage/summary.txt`.
Performance tests remain suspended. No commits or pushes were made.

### Save As to a new file

Editable sources offer File → Save As and Ctrl+Shift+S through GPUI's system
save dialog. Cancellation invokes no save command. Responses carry the
original document ID, so a tab change cannot redirect the save.
Ctrl+Shift+S is reserved for Save As; the optional log demo's Summarize action
now uses Ctrl+Shift+M. Duplicate shortcut rejection remains enforced.

The existing bounded source worker resolves the destination. The platform
reserves its resource key before publication and prevents closing that owner;
opening the reserved key activates the same document. A successful completion
changes the resource and title before delivering the saved session baseline.
Failed or stale completions cannot move another document or release its
reservation. The editor keeps its document ID, text entity and undo history.
Later edits remain dirty when an older snapshot finishes saving.

Publication reuses tempfile's no-clobber operation. Existing entries, including
symlinks, are refused; the original file is never modified by Save As. New files
use restrictive temporary-file permissions. The 4 MiB save limit still applies.
This increment does not provide overwrite confirmation, Save As for read-only
viewers, crash recovery or general reconciliation of file aliases. Resource
reservations compare normalized keys; path normalization belongs to providers.

Manual clarity review covers worker-only I/O, bounded admission, failed-save
ownership, stable document routing, atomic no-clobber publication and reuse of
the existing system-picker and editor APIs.

Local acceptance passed on 2026-11-18: formatting, strict Clippy, rustdoc,
239 workspace tests (including 24 headless UI tests), 22 native UI scenarios,
all 54 contributed controls and all five native negative controls. The separate
file-finder native workflow and its three negative controls also passed. Native
tests used XTEST input on an isolated Xvfb display with llvmpipe rendering;
screenshots, state snapshots and event evidence are retained in `target/e2e`
and `target/file-finder-e2e`.

Line coverage is 93.85% (7,873 of 8,389 lines) across platform, Projects, Cargo,
Ninja, logs and log-stats. This does not measure the whole application. Details
are in `target/coverage/summary.txt`; the full gate log is `target/check.log`.
Performance tests remain suspended. No commits or pushes were made.

### Discard confirmation

Closing a dirty document now opens the existing GPUI component dialog. Cancel,
Escape and Enter keep the edits; the Discard button closes without writing.
Exit and the window-close request confirm each dirty document individually.
Cancelling later in that sequence keeps the remaining documents open; earlier
explicit discards have already closed their documents.

The provider close hook now returns `CloseError`: unsaved edits carry an edit
revision, while blocked work cannot be discarded. The platform issues an opaque
single-use discard request bound to document identity and revision, rechecks the provider on
confirmation and rejects stale consent, including edits that have since been
saved or a save now in progress. Cancelled and superseded responses cannot be
replayed. Native providers use the same contract.

The modal, focus trap, backdrop and buttons are library components. The small
adapter supplies callbacks, semantic observations and the existing frame marker.
Manual clarity review covers ownership, stale responses, modal lifecycle,
single-document removal and sequential quit cancellation.
Local acceptance passed on 2026-11-18: formatting, strict Clippy, rustdoc,
233 workspace tests (including 23 headless interaction tests), all 22 native
XTEST workflows, 52/52 contributed controls and all five native negative
controls. The separate file-finder workflow and its three negative controls
also passed. The discard-dialog screenshot was visually reviewed.

Line coverage was 93.83% (7,537 of 8,033 lines) across platform, Projects, Cargo,
Ninja, logs and log-stats. Workbench, application, testkit and the file-finder
experiment are outside this measured scope. Evidence is in `target/check.log`,
`target/e2e/report.json`, `target/file-finder-e2e/report.json` and
`target/coverage/summary.txt`. The final full gate exited successfully; an
earlier run terminated with SIGTERM and was rerun, not accepted as passing.
Native tests used authenticated Xvfb `:97` with Mesa llvmpipe. Performance tests
remained suspended. No commits or pushes were made.

That slice did not change document paths or add recovery storage. Save As to a
new file is implemented above; crash recovery remains outstanding.

### Editable document sessions

Writable UTF-8 source files now support typing, undo/redo, dirty tab markers and
background Save through Ctrl+S or File → Save. The GPUI component owns text
editing and undo; immutable Rope snapshots feed a toolkit-independent
`DocumentSession`. Saved text and current edits are tracked separately, so an
older save cannot mark newer edits clean. Save completion never reloads the
editor or resets its selection/history. Initial read-only permission bits keep
the viewer read-only; Save also checks current permissions and filesystem access.

The existing source worker serializes reads and saves through its 16-job queue.
Each session admits one save at a time. A rejected/dropped request restores the
file handle and reports cancellation; failed saves retain edits and the old
baseline. Closure and normal Exit are rejected while changes remain unsaved or a
save is running. The same `can_close` hook is available to ordinary and typed
native providers. This initially required Save or undo; the discard-confirmation
slice above adds explicit consent to close without saving.

Linux replacement now preserves owner/group, permission bits, access ACLs and
extended attributes through rustix. It removes unwanted inherited attributes
from the temporary file and fails before publication if preservation fails.
Metadata buffers and aggregate attribute storage are bounded. The final check
also rejects detected identity/ctime changes. The check/rename interval still
cannot prevent every race with another process. Other platforms currently
reject replacement until their metadata implementation is supplied.

Reuse: Ropey is the same version already used by GPUI; there is no custom buffer,
undo engine or edit-event bus. Source edits use the existing typed native-event
transport, avoiding additions to the central view-event enum. The save action
remains in Projects alongside its source factory; extracting the editor plugin
is follow-up work once document ownership and recovery are established.

Limits: there is no crash recovery, autosave, Save As, external-change watcher,
versioned multi-file editing, independent split-view undo integration or alias
reconciliation yet. The 4 MiB file limit still applies to reading and saving;
oversized edits remain in memory and report a save error. Performance acceptance
is suspended. Normal close protection is not crash-recovery evidence.

Manual clarity review covers snapshot ownership, single-flight saves, admission
failure, worker lifetime, nonblocking completion delivery, metadata preservation,
close/quit protection and reuse of the component's actual edit events/history.
Local acceptance passed on 2026-11-18:

- Formatting, strict Clippy and rustdoc passed, including a final static check
  after the last source/documentation adjustments.
- All 231 workspace tests passed, including 23 headless interaction tests.
- All 22 native XTEST workflows passed; all 52 contributed controls were
  exercised and all five negative controls were detected. The editing workflow
  verified shortcut/menu Save, dirty close/quit refusal, undo/redo after Save
  and reopening the saved file. Its screenshots were visually reviewed.
- The separate file-finder native workflow and three negative controls passed.
- Line coverage was 94.04% (7,426 of 7,897 lines) across platform, Projects,
  Cargo, Ninja, logs and log-stats. Workbench, application, testkit and the
  file-finder experiment are outside this measured scope.

The full gate exited successfully. Evidence is in `target/check.log`,
`target/e2e/report.json`, `target/file-finder-e2e/report.json` and
`target/coverage/summary.txt`. Native tests used authenticated Xvfb `:97` and
Mesa llvmpipe; these are correctness checks, not performance measurements.
Performance tests remained suspended. No commits or pushes were made.

### Previous file-persistence slice

The first editing slice adds `platform::document_file::DocumentFile`: a bounded
UTF-8 snapshot with four operations (`open`, `path`, `text`, `save`). The existing
Projects source loader now uses it. Immutable `Arc<str>` snapshots cross the
worker/UI boundary without copying; the GPUI editor still owns its display state.
At that stage the application remained read-only; Save was only a platform
operation. The editable-session slice above supersedes that limitation.

Replacement uses the existing `tempfile` dependency. It preserves text bytes and
ordinary permissions, checks the baseline before replacement, and rejects known
conflicts, missing files, invalid/oversized contents, read-only files, symlink
replacement and Unix hard links. Ten filesystem contract tests cover these cases.
The baseline advances only on success; old snapshots remain valid.

That review identified metadata preservation, document ownership, serialized
background saves and dirty-close handling as prerequisites for editable files.
The editable-session slice above implements these. Recovery and directory
durability remain outstanding. The disk check and rename are not a
compare-and-swap against other processes.
The component's `set_value` clears undo and resets selection/scroll, so it now
runs only for initial content; save completions never reload the active buffer.
The component currently keeps text, history and view state together, so shared
split/diff editing needs a focused reuse assessment before promising independent
views with one history.

Manual clarity review covered the four-operation interface, immutable snapshot
ownership, worker-only I/O, failure ordering, byte limits, replacement semantics
and the existing loader's cancellation/lifetime handling. No new worker service,
custom editor buffer, undo engine or file-writing dependency was introduced.

Native validation exposed a separate capture race: pixels encoded frame 41 and
matched the preceding snapshot, but a subsequent snapshot advanced to frame 43.
Requiring both snapshots to agree rejected valid evidence during repainting.
Capture now requires the decoded marker to match the preceding snapshot exactly;
it does not require the UI to stop repainting afterward. The stale-pixel check
remains unchanged. Unmatched capture failures now preserve raw pixels, both
snapshots and traces, clearly separated from accepted checkpoints. The first
failed run is retained in `target/document-foundation-first-e2e`, the reproducer
in `target/document-frame-diagnostic`, and the passing focused regression in
`target/document-frame-fixed`.

The faster capture also exposed lost focus when a search input opened a source
document: focus still belonged to the input removed by the next layout, so the
next shortcut had no workbench key context. Document activation now restores
workbench focus before honoring explicit plugin focus requests. The headless
Enter-submission test now switches repeatedly between a filter and a source
using real key events. The failed native finder evidence is retained in
`target/document-focus-failure`.

The complete local gate passed on 2026-11-18 after both fixes (verified exit 0):

- **222 workspace tests**, including 22 headless tests and ten new filesystem tests.
- **21 native workflows**, **50/50 controls**, and **five detected negative controls**.
- **One file-finder native workflow** and **three detected negative controls**.
- **94.24% line coverage** across platform, Projects, Cargo, Ninja, logs and
  log-stats (7,625 lines, 439 missed). The new file module measures **96.77%**
  (62 lines, two missed). Neither number measures the whole application.
- Formatting, strict Clippy, rustdoc and manual clarity review passed.

Final evidence is in `target/check.log`, `target/e2e/report.json`,
`target/file-finder-e2e/report.json` and `target/coverage/summary.txt`.
The run used authenticated isolated Xvfb with llvmpipe. Performance tests stayed
disabled. No commits or pushes were made; ktask remains idle.

### Previous accepted baseline

The file-finder experiment lives in `experiments/file-finder` and is not bundled
into the IDE. Its library choice, runnable command, integration findings and
remaining performance decision are recorded in [decisions](decisions.md#file-finder-prototype-and-decision).
The focused run passed nine correctness/lifecycle tests. Native interaction
evidence is separate from the normal application in `target/file-finder-e2e`;
the full gate passed with that experiment included on 2026-11-18 (exit 0).

The required local check is `bash scripts/check.sh`. It includes formatting,
strict Clippy, rustdoc, unit/integration/headless/source-rule tests, native XTEST
workflows and line coverage. Its evidence lives in `target/check.log`,
`target/e2e/report.json`, per-checkpoint images/snapshots/traces and
`target/coverage/summary.txt`. Performance workloads are excluded.

The complete local checks passed on 2026-11-18 using
`CARGO_BUILD_JOBS=2 bash scripts/check.sh` to limit link-job memory pressure:

- **212 unit/integration/headless/source-rule tests**, including 22 headless tests
  and nine file-finder correctness/lifecycle tests.
- **21 native XTEST workflows**, **50/50 contributed controls** exercised.
- **Five existing negative controls detected**, including stale pixels and lost/misaimed input.
- **One additional file-finder native workflow** and **three additional negative
  controls** passed, with separate evidence in `target/file-finder-e2e`.
- **94.16% line coverage** over the stated six-crate scope: 7,586 lines, 443 missed.
- Formatting, strict Clippy and rustdoc passed.

The gate log reaches `check: passed`, both native reports record success, and
process exit 0 was verified. This run used an authenticated external Xvfb display
with Mesa llvmpipe, following the desktop-modifier interference documented in
[testing](testing.md). No performance test was run.

Inspected images show logical project groups, two open projects, a line-numbered
read-only editor and selected source text. Workbench checkpoints have matching
frame-tagged semantics and trace. System-picker images capture the external
portal window and do not carry the workbench frame marker. Earlier exploratory
runs are retained separately and are not the final evidence.

Manual clarity review covers service/registry ownership, shell event routing,
bounded task delivery, import publication and cancellation, project/task session
ownership, logical roots, the system-picker adapter and editor lifecycle. The
file-finder review covered library ownership off the UI thread, coalesced requests,
catalog generations, cancellation, candidate bounds, watcher errors and the
shared document-filter Enter event.
Source scanners and documentation lints complement that review; they do not
prove semantic clarity.

## Remaining limits

- Native plugins are statically linked and trusted. There is no hot-unload or
  panic-rollback contract. Process protocol 0 has no request cancellation or
  sandbox and cannot supply views.
- Refresh, cross-restart project restoration and launch
  configuration UI remain future features. Document IDs identify one live
  platform lifetime; provider IDs do not yet define refresh reconciliation.
- Dock sizes remain fixed. Crash recovery, Save As overwrite confirmation, Tree-sitter, LSP, debugger, structured
  test results and coverage UI are not implemented.
- Native UI tests exercise selected X11 workflows on Xwayland and Xvfb. They do not establish
  native Wayland, IME, accessibility, scaling or other-platform support. Declared
  observation checks cannot detect a renderer's undeclared interactive elements.
- Startup state loading remains synchronous; project discovery runs on a worker.
  A filesystem call
  already in progress cannot be forcibly cancelled; a timed-out shutdown write
  may finish later. State directories assume one live application owner.
- Line coverage measures platform, Projects, Cargo, Ninja and the two log plugins,
  excluding the workbench, application and test kit.

See [refactoring review](refactoring-review.md) for the disposition of each finding
and the Cargo adapter decision.
