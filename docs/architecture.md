# Architecture

## Layers

```
apps/demo ──────────────► crates/workbench (GPUI) ──► crates/platform
   │                                                     ▲
   ├──► plugins/projects ────────────────────────────────┤
   ├──► plugins/cargo ──► plugins/projects ──────────────┤
   ├──► plugins/ninja ──► plugins/projects ──────────────┤
   ├──► plugins/logs ────────────────────────────────────┤
   └──► plugins/log-stats ──► plugins/logs ──────────────┘

packages/*                       data and processes loaded at run time
plugins/external-sample          separate executable, JSON lines on stdio
crates/testkit                   tests and gate runners; depends on all of the above
```

- **`retrowork-platform`** has no UI toolkit dependency. It resolves modules,
  activates them, holds what they contributed, and keeps workbench state
  (open documents, active views, status, output, event trace). It also
  persists state and runs process plugins.
- **`retrowork-workbench`** draws a `Platform` with GPUI and turns pointer and
  key input into platform commands and view events. It contains no feature
  logic.
- **Bundled modules** (`plugins/logs`, `plugins/log-stats`) depend only on the
  platform. They describe content with view models (tree, table, text,
  properties); they never see GPUI.
- **The demo** composes these: bundled modules, packages from a directory,
  a state file, one window.

## Modules and contributions

A module has a manifest (id, version, kind, required platform API version,
dependencies, extensions) and an `activate` function. The platform:

1. Resolves manifests: duplicate ids, incompatible API versions, missing or
   mismatched dependencies and cycles reject a module, and a rejected module
   rejects its dependents. The order is deterministic and independent of
   registration order.
2. Activates accepted modules, core first, dependencies before dependents.
   Activation is transactional: contributions are staged and committed only
   if `activate` succeeds and every reference resolves.
3. Lists rejected modules with their reason in the Output, so a missing
   feature is explained.

A module contributes through `ActivationContext`: commands, menus, menu
items, toolbar items, key bindings, docked views, document factories,
services (typed APIs for other modules) and extension points (data hooks that
other modules and packages fill). A menu item, toolbar item, key binding or
extension may only refer to the module itself, the core, or a declared
dependency, so removing a module rejects its dependents instead of leaving
dead controls.

Services also retain their owner. Activation checks service dependencies;
command handlers, availability predicates, document/view events and queued
worker completions retain module-scoped access. Only the host has unrestricted
service access. `Registry<T>` stages contributions and retains their owner
through disposal. `ModuleResource` provides explicit, nonblocking shutdown
in reverse activation order, including resources staged by rejected modules.
Closing the platform rejects commands and input, closes the inbox, then stops
resources. Source loading and project tasks participate in this lifecycle.
This is trusted native code, not panic rollback, isolation or hot unload.

## Module kinds

| Kind | Code | Trust | Distribution today |
|---|---|---|---|
| Bundled | Rust, compiled into the application | full | Built with the application. A third party needs a custom build. |
| Declarative package | none; a `package.toml` with data | none needed | A directory in the packages folder. |
| Process package | any executable | runs with the user's privileges, out of process | A directory with a manifest and an executable. Protocol version 0, experimental. |

Bundled crates are not installable community plugins. The installable route
is packages; see [plugins.md](plugins.md) for both package kinds and the
process protocol.

## Front-end contract

A front end:

- draws `platform.contributions()` (menus, toolbar, dock tabs) and
  `platform.shell()` (documents, active views, status);
- asks providers for content with `view_content` and `document_content`;
- reports input as `platform.execute(command, args)` or
  `platform.view_event(target, event)`;
- calls `platform.pump()` when the inbox waker fires, so background results
  are applied on the UI thread;
- honours `shell.take_focus_request()` and `shell.quit_requested()`.

## Workbench and test observation

Each drawn control registers a semantic element: a stable id, its role,
label, value, selection and focus, and the bounds layout actually gave it,
clipped to what is visible. Elements register during GPUI's prepaint, after
layout and before painting, and the registry is rebuilt for every frame. A
snapshot describes layout state; by itself it does not prove which pixels
were captured. Tests find controls by id and click their laid-out position,
so a click goes through real hit testing.

Automation windows paint a small binary frame marker into the actual GPUI
scene. Screenshot capture decodes that marker, including its complement row, and
requires it to match the snapshot requested immediately before capture. The saved PNG, snapshot and trace (bounded by that frame's trace sequence)
form one checkpoint. A stale-pixel negative control must fail this check.
This proves correspondence in the X-server image. It does not claim physical
monitor scan-out, compositor timing or native Wayland coverage.

The workbench also records raw input (`ui.mouse_down`, `ui.key_down`), each
operated control (`ui.activate`) and every command in the trace. A test can
therefore show that an outcome followed from input, in order.

With `--automation-socket`, the demo serves an owner-only Unix socket that
returns the snapshot, the trace, the list of contributed controls and the GPU
adapter the window renders with, and can inject input into GPUI's event
dispatch. It cannot run commands or
change state. With `--exit-report`, the demo writes its trace and control
list when it quits normally.

## Projects and build tools

`plugins/projects` owns everything that is the same for every build tool:
Import/Open/Close, portable project configuration, the logical Projects tree, the standard actions Build, Run,
Test and Stop, one running task at a time, Build Output with the task's
state, Problems, and a source document opened at a diagnostic's
line. A build tool contributes only a `ProjectProvider`, registered with the
`ProjectRegistry` service:

| Provider method | Cargo |
|---|---|
| `actions()` | shared Build/Run/Test declarations |
| `detect(directory)` | `Cargo.toml` exists |
| `discovery(directory)` → tool invocation | `cargo metadata --format-version 1 --no-deps` |
| `parse_discovery(output)` → `Project` (units, targets) | packages and their targets |
| `logical_roots(project)` → categorized paths | conventional roots plus workspace manifest and lockfile |
| `availability(project, action, scope)` → enabled or reason | validates the selection without constructing a process |
| `plan(project, action, scope, options)` → tool invocation | `cargo build\|run\|test [-p … --bin …] --message-format=json` |
| `interpret(line)` → output lines and diagnostics | `compiler-message` JSON to located diagnostics |

Providers are pure: they describe invocations and read output, and never
start processes or touch the UI. A Maven, Go or Gradle provider implements
the same methods; the scope (project, unit, target) and actions are
not Cargo-specific. Ninja proves this with real `ninja -t targets all`
discovery, Build/Run/Test and its custom `ninja.clean` action. Shared actions
have common IDs; custom IDs belong to a provider namespace and automatically
contribute commands/menu items. Options are decoded and validated by each
provider at execution; Cargo supports profiles, features, release builds,
explicit workspace builds/tests and program/test arguments. There is no
launch-configuration UI yet.

The current session is separate from pending discovery. Its tree stays visible
while another directory is discovered; actions wait for discovery to finish.
Each discovery has a generation and cancellation token. Only its matching,
uncancelled result can add or select a project session. Each project retains its
selection, output, diagnostics and task ownership. Stop cancels discovery or
the running action. Tree selection is validated into a typed scope; stale
IDs are rejected without falling back to a project-wide action. Units and
targets have provider-owned identities; target source paths are optional.
Discovery rejects duplicate or empty IDs. These identities do not yet define
refresh reconciliation or session restoration across restarts.

Discovery runs on a worker thread with `task::capture_with_cancel` (bounded
size and time, with process-group cleanup on cancellation). Actions run with `task::spawn`; the provider interprets output on the
runner thread and the results reach the UI through the inbox in batches.
Availability is a cheap, side-effect-free provider query. Invocation planning
runs only when executing an action and validates the scope again.

Source loading uses the platform's `DocumentFile` for bounded UTF-8 reads and
shares immutable `Arc<str>` text with the UI. `DocumentFile` also offers
conflict-checked atomic replacement through `tempfile`; it does not own editor
undo or recovery. The Projects plugin submits saves to its file worker.
All its filesystem operations must run on a worker. Full-file snapshots are a
bounded I/O boundary, not the proposed representation for per-keystroke edits.

Source documents own their buffers. One background worker accepts at most 16
queued reads/saves, enforces the 4 MiB limit while reading, and publishes through
weak delivery handles. Closed documents retain no buffer in the workspace.
Open requests can carry a view event, so diagnostic navigation reaches the
actual new or existing document even before its read completes. Each open document has a non-reused `DocumentId` for
its lifetime in the platform. Input and asynchronous destinations use that
identity, so closing a tab cannot redirect an old callback to another tab.
Shutdown closes source admission and discards queued reads; a filesystem call
already in progress may finish afterward.

Save As resolves a new destination on that worker, then reserves its normalized
resource key on the UI thread before writing. Completion commits the tab identity
before delivering the saved baseline. Failed writes release the reservation and
keep the original resource; later edits and editor history remain intact. The
system picker captures the requesting document ID. Existing destinations are
refused through tempfile's no-clobber publication.

Reload uses the same bounded reader. `Shell::discard_then` retains a provider
event behind a single-use document/revision confirmation when edits are dirty;
busy documents reject it. Accepted events run with their document owner's service
access. A reload captures the session revision before reading and installs the
result on the UI thread, including for inactive tabs. The session rejects changed
paths, newer edits, concurrent saves and read-only replacements. Failed reads keep
the current text and undo history; a successful read updates the baseline and
uses the editor library's reset operation to clear obsolete undo history.

## Running tools

`retrowork_platform::task` runs a tool in its own process group and streams
its stdout and stderr as lines. Lines are cut at 8 KiB; at most four batches
of up to 512 lines wait to be consumed, and a batch is held until the UI has
applied it, so a tool that floods its output is slowed by its own pipe
instead of growing memory. Partial batches are delivered after 30 ms.
The task owns its process group: when it ends, by itself or by
cancellation, every process still in the group is killed before `Finished`
is reported. Cancelling sends `SIGTERM` to the group and `SIGKILL` two
seconds later if the task has not finished, also when the leader has already
exited and only descendants remain; a dropped handle of an unfinished task
kills the group at once. If the leader exits while a descendant keeps the
pipes open, the descendant is killed two seconds after the exit, so a task
always ends with exactly one `Finished` event. The leader is reaped only
after its group has been killed, so the group id cannot be reused by an
unrelated process while signals may still be sent. If starting fails part
way, for example when a thread cannot be created, the group is killed and
reaped before the error is returned.

`task::capture` has one deadline for the whole run, covering the leader's
exit and reading both pipes; it applies the same leftover rule and kills the
group on every error. Processes that leave the group (`setsid`, `setpgid`)
are not stopped or waited for; if one keeps a pipe open, the thread reading
that pipe stays alive until it exits, but the task or capture still ends on
time. Build Output keeps the newest 10,000 lines and says how many were
dropped.

Discovery (`cargo metadata --no-deps`) reads manifests and does not build or
execute project code. Build, Run and Test do: Cargo runs build scripts and
procedural macros, and Run and Test execute the program and its tests.

## Persistence

State (open documents and their filters, active document, active views) is
one JSON file. Saves write a uniquely named temporary file, fsync it and
rename it over the target, then fsync the directory on Unix. A file that
cannot be read is moved aside, never deleted; a file from a newer schema is
left untouched and the session does not save over it.

Saves run on one background writer. It keeps the active snapshot and the
newest pending snapshot, never overlapping writes. Failed snapshots remain
dirty and retry with capped exponential backoff; repeated error messages
are suppressed separately from save progress. An explicit flush waits for
the requested revision, a new failure, or its deadline.

Normal File > Exit, the quit shortcut, and window close stop input and close
the completion inbox before a background shutdown flush, with a two-second
wait. The final GPUI quit hook has a shorter fallback wait within GPUI's
200 ms shutdown allowance. Shutdown rejects new snapshots and prevents
further writes after the active filesystem call returns. OS filesystem calls
cannot be forcibly cancelled, so a timed-out write may finish afterward.
Startup state loading remains synchronous. One live application owns a state
directory; cross-process locking is not implemented.

## Process plugins

The host never blocks the UI thread on a plugin. One worker thread per plugin
owns the process; requests queue in a bounded queue (one running, four
waiting, more refused at once). One deadline covers queueing, start-up,
handshake, writing and the reply; writes go through a separate thread so a
plugin that stops reading cannot hang the host. Plugin output is bounded per
line and per buffer; anything but the reply to the current request is a
protocol violation. A timeout, exit or violation kills and reaps the process;
the next request starts a new one. Dropping the host kills the process at
once. Child processes a plugin starts itself are not tracked.

## Bounded UI scheduling and native extensions

The shared inbox bounds admission to 256 callbacks. `try_post` never waits;
`post_blocking` applies backpressure on workers and refuses the creating UI
thread. Both return rejected work. Closing the platform wakes producers and
drops pending work outside the queue lock. Only the host receives queue-drain
and wakeup capabilities. Pumps start at most 32 callbacks within a 4 ms
scheduling window; individual callbacks must remain short and cannot be
preempted.

The workbench coalesces wakeups through a bounded channel and waits for a
GPUI next-frame callback between batches. A headless interaction regression
fills the queue and verifies pointer and keyboard input before it empties.
It simulates frame delivery explicitly, as GPUI's test platform has no native
frame loop. This is a functional scheduling test, not a performance result.

Native renderers declare a `NativeModel` and its event type, with checked type
erasure only at the registry boundary. `ViewContext` exposes scoped event,
focus and observation helpers, without exposing `Context<Workbench>`.
Renderers declare control IDs; snapshots reject missing or duplicate IDs.
The sample exercises pointer, Enter and Space through actual controls.
Commands register identity, title, handler and availability together through
`CommandDefinition`; availability is read while rendering and checked again
before execution.

Projects is divided into registry, discovery, action execution, source loading
and view adapters. The workbench separates input/lifecycle, shell rendering
and content rendering. These are modules inside their existing crates, with
the same application entry points.

## System dialogs and reuse

The workbench uses GPUI's `prompt_for_paths` for directory selection. Linux uses
its existing desktop file chooser portal. The plugin requests a folder through
`FolderRequest`; confirmation supplies a directory to its command, cancellation
changes no project state, and errors are visible. No custom file browser is kept.
The source editor uses GPUI's editor component rather than implementing
text layout, selection, navigation or clipboard handling in the workbench.
