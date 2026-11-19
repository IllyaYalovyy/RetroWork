# Decisions

## Search libraries

Status: disposable prototype implemented, 2026-11-18; performance acceptance pending.

The first Search plugin finds files only. Use the high-level
[Nucleo](https://github.com/helix-editor/nucleo) crate for fuzzy matching and its
background workers. It is used by Helix and provides Unicode-aware matching.
It is an in-memory candidate matcher, not a persistent full-text index.
Use [ignore](https://docs.rs/ignore/latest/ignore/) for file traversal and ignore
rules. Keep project ownership and relative paths attached to candidates.
Do not implement a matcher, ignore parser or indexing engine locally.

For later persistent text indexing, the leading Rust candidate is
[Tantivy](https://github.com/quickwit-oss/tantivy): an embedded, MIT-licensed
engine with incremental and multithreaded indexing, memory-mapped indexes,
ranked retrieval and field filters. This is a fit recommendation, not a claim
that it is the fastest engine for our workload.

Source-code search needs more than word search. Tantivy's
[RegexQuery](https://docs.rs/tantivy/latest/tantivy/query/struct.RegexQuery.html)
matches indexed terms, not arbitrary spans of original source. Its built-in
[NgramTokenizer](https://docs.rs/tantivy/latest/tantivy/tokenizer/struct.NgramTokenizer.html)
does not preserve ordinary positional relationships between grams. Verify
substring, punctuation, short-query and line-location behavior before adopting
it for content search; do not assume its default analyzer supplies IDE semantics.

Alternatives reviewed:

- [Zoekt](https://github.com/sourcegraph/zoekt) already implements indexed
  substring and regex code search. It is written in Go; adopting it introduces
  a separate runtime/process integration instead of an embedded Rust library.
- [SQLite FTS5](https://sqlite.org/fts5.html#the_trigram_tokenizer) supplies a
  ready-made substring index and is a serious simplicity alternative if a C
  dependency is acceptable. Trigram full-text queries have a minimum length
  of three Unicode characters; some patterns fall back to scanning.

No performance runs were made. With exclusive host access, compare cold and
warm queries, initial indexing, single-file changes, deletion/rename, memory
and disk footprint on real source trees before making performance claims.
The file-finding milestone does not add a full-text engine dependency.

### File-finder prototype and decision

`experiments/file-finder` is a separate executable, not a bundled IDE feature.
The lockfile resolves Nucleo 0.5.0, ignore 0.4.34 and notify 8.2.2. The normal
application does not depend on the experiment. The temporary UI adapter reuses
the workbench table, GPUI text input, source viewer and XTEST test driver. The
only shared UI change forwards Enter from a document filter to its existing
`InputSubmitted` event.

Run against the actual repository:

```sh
cargo run -p retrowork-file-finder-prototype -- --project /home/etf/Projects/RetroWork
```

Repeat `--project` for separate roots. `--generated relative/path` explicitly
classifies generated directories across the supplied roots; the experiment's
default is `generated`. Press Ctrl+P, type a filename or path fragment, use
Alt+Up/Down to select and Enter to open. A project name can be another query
term. Tools exposes refresh, generated-source inclusion and closing a search
project. These are experiment controls, not the final interaction design.

**Functional recommendation:** retain Nucleo, ignore and notify for the next
file-finding implementation. They cover matching, ignore parsing and platform
notifications without a custom search engine. This is not performance approval
and does not select Tantivy for content search.

Findings that affect the production approach:

- Nucleo manages matching workers but its `tick(0)` cancellation path and
  destructor can wait for a worker. The experiment therefore owns Nucleo on a
  background coordinator, not the UI thread. The UI receives bounded snapshots.
- Notifications alone are insufficient to drive the matcher: a callback can
  arrive just before its worker releases the lock. A native test exposed a
  stuck Updating state when closing the last project. The coordinator now also
  polls every 16 ms while matching is pending, and sleeps when idle. Repeated
  empty/nonempty transitions are regression-tested.
- Nucleo has append/restart semantics, not individual candidate deletion. This
  prototype replaces the candidate set after changes. Before production, measure
  that cost under build-generated event bursts and large repositories; use an
  existing notify debouncer if coalescing needs a time window. Do not implement
  an incremental matching index locally. The experiment watches entire roots:
  writes inside excluded build output and ordinary content edits can still cause
  a rebuild. Production needs event filtering as well as coalescing so builds
  do not continually invalidate an otherwise unchanged filename list.
- File creation, rename, deletion and ignore changes trigger reconciliation.
  Refresh also rebuilds from disk; a test disables notifications entirely and
  verifies recovery. This does not prove every operating-system overflow mode.
- Project/catalog generations invalidate old candidates immediately, including
  when a project closes during discovery. File contents are never indexed.
- Candidate enumeration has global entry/path-byte limits, a depth limit and
  a 200-result UI limit. Truncation and errors are visible. These are resource
  safeguards, not measured performance budgets.
- Generated files are explicit metadata and stay distinct in the results.
  Inclusion still respects ignore rules; build output (`target`), `.git` and
  `.retrowork` are excluded. Language-provider classification and searching
  ignored generated roots remain later work.
- Final open validation still performs a synchronous filesystem check in this
  disposable adapter. Production must put validation on the asynchronous opener
  and recheck project ownership when its result returns.
- Native checks on nested Xwayland can receive desktop modifiers on a shared
  host. A captured trace showed `super-r` instead of `r`. Use an isolated
  virtual display for repeatable functional acceptance; the same native workflow
  passed with Xvfb and llvmpipe. This does not replace later GPU performance tests.

The resolved crates declare Nucleo MPL-2.0, ignore Unlicense OR MIT, and notify
CC0-1.0. Preserve dependency
licenses/notices when packaging; this experiment does not copy their source.
No comparative benchmark or memory measurement has been performed. Retain the
existing performance suspension until exclusive host access is available.

## UI stack: GPUI through GPUI Kit 0.10.0

Status: accepted for the foundation, 2026-11-17.

**Decision.** Use GPUI through the `gpui-kit` crate, pinned to `=0.10.0`
(imported as `gpui`). It brings the matching GPUI snapshot `gpui-pre =0.3.12`,
`gpui-base` and `gpui-component` 0.10.0, all Apache-2.0. `Cargo.lock` is
committed and every gate uses `--locked`.

**Why.** GPUI is the toolkit chosen for the project. GPUI Kit pins a GPUI
snapshot together with a component library tested against it, and ships a
UI test harness that dispatches real pointer and key events through GPUI's
hit testing and focus. The official `gpui` crate on crates.io (0.2.2) is
much older than the snapshot the components need.

**Checked by building and running, not only by reading.**

- The pinned set resolves and builds on Fedora 45 with Rust 1.99.1.
- GPUI on Linux links `xkbcommon-x11`; the development package is required.
- The demo runs as an X11 client of a nested Xwayland and renders with Vulkan.
- XTEST input reaches GPUI windows and the X server returns rendered pixels
  for screenshots.

**Used from the kit.** The text input (`Input`, `InputState`) for the filter
field, and the theme. Menus, tabs, tree, table and docks are drawn by the
workbench with plain GPUI elements, because each needs a stable semantic id
and a NetBeans-like look; replacing them with kit components later is
possible.

**Accepted risks.**

- The GPUI snapshots are published by one person's crates.io account and
  change often; an upgrade is a deliberate change of the pinned set, followed
  by `bash scripts/check.sh`.
- GPUI needs a Vulkan driver (software lavapipe works).
- No Zed editor code is used or copied; the workbench is written against
  GPUI's public API.

## Native UI testing route

Status: accepted, 2026-11-17.

**Decision.** Native tests start a private nested Xwayland with its own
authority cookie, run the demo as its X11 client, send input with XTEST and
read screenshots back from the X server. The application is observed through
an opt-in Unix socket that exposes element snapshots and the trace, and
writes an exit report when it quits.

**Why.** It drives the shipped executable through the operating system's
input path without touching the user's desktop, needs only Xwayland (already
present in Wayland desktops), and fails clearly when unavailable. Wayland
compositors offer no standard input injection, and no headless Wayland
compositor was installed.

**Limits.** The test server has no window manager; the runner gives the
window keyboard focus itself. Native Wayland windows, input methods and
accessibility are not covered.

## Plugin tiers

Status: accepted, 2026-11-17.

Bundled Rust modules, declarative packages and process packages share one
manifest and dependency model but differ in trust: bundled modules are
compiled in and fully trusted; declarative packages carry no code; process
plugins run out of process behind a bounded, deadline-enforced protocol. The
process protocol is an early experiment to prove the route for independently
built plugins before the APIs settle; it is not a stable API.

## Projects and build-tool providers

Status: accepted, 2026-11-18.

**Decision.** Project support is split into a build-tool independent
Projects module and one pure `ProjectProvider` per build tool. The module
owns the UI, the single running task, output, problems and source viewing;
a provider only describes tool invocations (discovery and the standard
actions Build, Run, Test on a project, unit or target) and interprets output
lines into text and diagnostics. Processes are run by the platform's task
runner, never by a provider.

**Why.** Every provider then gets the same streaming bounds, cancellation,
state reporting and tests, and a provider can be tested without processes or
UI. Cargo is the first provider; nothing in the workbench or the Projects
module refers to Cargo.

**Limits.** Providers are Rust code in bundled modules; packages cannot
contribute one yet. One task runs at a time. Discovery uses
`cargo metadata --no-deps`, which needs no network and executes no project
code; builds of projects with dependencies may need the network, and Build,
Run and Test execute project code (build scripts, procedural macros, the
program, its tests).
