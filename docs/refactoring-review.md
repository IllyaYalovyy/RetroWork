# Foundation refactoring review

The initial static review on 2026-11-18 identified the findings below. The user then authorized implementation. This document retains the original findings and distinguishes completed work from open contracts.

## Implementation status

Both implementation passes are complete; acceptance of the current revision is
recorded in [status](status.md). The original review below remains historical.

| Finding | Current disposition |
|---|---|
| 1. Document ownership/navigation | Open documents own source buffers; open requests carry navigation. Non-reused `DocumentId` destinations prevent tab removal from redirecting events. Reopen tests verify the diagnostic line and buffer release. |
| 2. Background persistence/source I/O | A serialized writer coalesces and retries saves. A bounded source loader reads off the UI thread. Shutdown closes admission, cancels queued work and bounds flush waiting; active filesystem calls cannot be interrupted. |
| 3. Project state/identity | Current sessions and pending generations are separate. Discovery is cancellable, stale results/selections are rejected, and unit/target IDs belong to providers. Refresh and multi-project restoration remain feature work. |
| 4. Commands/actions | One command descriptor owns availability/execution. Providers declare shared/custom action IDs and validate configuration. Cargo supports concrete options; Ninja proves custom actions and non-Cargo target policy through the shared lifecycle. |
| 5. Ownership/disposal | Services retain owners. Activation, handlers, predicates, document/view events and queued completions enforce declared dependency access. Typed registries retain contributors and dispose registrations. Resources shut down in reverse activation order, including rejected activations. |
| 6. Native views | Rust models/events stay typed until the checked registry boundary. Renderer context has view-scoped listeners, focus and observation without the workbench entity. Missing/duplicate observations fail; the native counter covers pointer, Enter and Space. |
| 7. Queue/scheduling | Explicit `try_post` and worker-only `post_blocking` return rejected work. Drain capability stays private. Batches yield through GPUI's next-frame callback. A full-queue interaction test checks input before draining, alongside backpressure/shutdown tests. |
| 8. Screenshot evidence | An automation-only binary marker identifies the painted frame. Capture matches the pixels to the snapshot requested before capture and a frame-bounded trace. A stale-pixel negative control rejects the previously possible mismatch. This proves captured X pixels, not monitor scan-out. |

Projects is split into registry, discovery, actions, source loading and view
adapters. Workbench input/lifecycle, shell rendering and content rendering are
separate modules. Public application entry points and crate boundaries remain.

The second-provider exercise exposed a concrete Cargo assumption: not every
target has one main source file. `Target::source` is now optional, and target IDs
are independent of labels. Ninja also demonstrates that custom action registration
can contribute a menu/command without editing the shared standard-action list.
Ninja was selected because it is an available real tool with different project
semantics; Go, Maven and Gradle are not implied to be implemented.

The frame-scheduling regression caught another concrete boundary: GPUI's animation
request API requires render context, while `on_next_frame` itself schedules frame
delivery. The pump uses only the latter; headless tests explicitly deliver frames.
Native tests also exposed duplicate keyboard activation: GPUI already turns
Enter/Space key releases into clicks. The sample now uses that single route.
The headless driver previously omitted key-up events; adding them reproduced
the failure before the fix. A new focus request is protected until its first
layout so a background completion cannot mistake the newly shown field for
a removed control. The failing native evidence is retained in
`target/e2e-input-regressions`.

## Review decisions

- Keep the current platform/workbench/plugin split and pinned UI stack. No new
  task framework, general event bus, schema language or file-splitting crates.
- Keep domain errors typed where code distinguishes outcomes (`TaskOutcome`,
  command errors, queue rejection). Provider availability/configuration failures
  currently become UI explanations, with no caller branching on their text. An
  additional generic provider-error hierarchy would not improve current behavior.
- Keep the current Cargo metadata/message parser in this pass after evaluating
  [`cargo_metadata`](https://docs.rs/cargo_metadata/latest/cargo_metadata/).
  Its typed `Metadata`, `Message` and extensible `TargetKind` are a good fit for
  the next schema expansion. Deserialize already bounded captured output;
  do not replace the task runner with `MetadataCommand::exec`, which would
  bypass our cancellation, output bounds and process ownership. Migration is
  useful follow-on work before adding Cargo schema handling, not a new parser
  framework. This pass adds action configuration without expanding that schema.
- Keep one current acceptance source in [status](status.md). Earlier findings and
  test counts describe their own revisions, not current acceptance.

Manual clarity review covered the existing ownership, callback and shutdown
paths as well as new registry/service scopes, typed events, action planning,
source cancellation, frame matching and module boundaries. It removed redundant
clones and repeated target-kind parsing, and corrected observation documentation.
Formatting, source rules and rustdoc check objective omissions, not comment value.

## Verdict

The reviewed contracts now have concrete implementations and regression coverage.
The next feature work can consume them without another framework rewrite, with
the full local gate passing. The plugin API remains experimental: native isolation,
hot unload, refresh reconciliation and cross-platform UI acceptance are not solved
by these changes. Resizable docking and an editable document with recovery are
appropriate next consumers of the tightened interfaces.

The findings below describe the code at the initial review.

## 1. Correct document ownership and navigation

**Priority: correctness first.** `plugins/projects/src/lib.rs` stores strong `Rc<SourceDocument>` references in `State.sources`. Closing a document removes its tab but never removes this reference. Distinct closed files therefore remain in memory for the workspace service's lifetime.

There is also a navigation defect visible in the code path: open a diagnostic source, close it, then select its diagnostic again. `reveal` selects the retained old document and does not queue the line for a new document. `SourceFactory::open` creates a fresh document without that selection.

Let the open-document lifecycle own source buffers. Use weak lookup references or explicit close removal for auxiliary lookups. Carry navigation location in the open/reveal request instead of a separate path-to-line mailbox. Introduce a stable document identifier before asynchronous navigation; vector indices are presentation positions, not durable identity.

Acceptance: repeat open/close/reveal, verify the selected line through the real UI, and verify closed buffers are released unless another explicit owner retains them.

## 2. Move persistence and source I/O off the UI thread

**Priority: responsiveness and durability.** `Platform::save_if_changed` calls the synchronous `StateStore::save`, including file and directory synchronization. It runs after commands, view events and background pumps. The pump's callback scheduling window therefore does not bound the whole turn. Source reading and conversion into text lines also happen synchronously in `SourceFactory::open`.

A failed state save currently sets `last_saved` to the failed state to suppress repeated error messages. That also prevents retrying that unchanged state after storage recovers. Error deduplication and successful persistence must have separate state.

Use one serialized persistence worker with coalesced immutable snapshots, explicit dirty/saved revisions, bounded retry and a shutdown flush contract. Keep the existing atomic-write implementation. Use the existing task/background mechanisms for source loading; enforce a byte limit while reading, not only with a preliminary metadata check.

Acceptance: a deliberately blocked writer must not block UI input; a failed save must remain dirty and retry; an older write must never overwrite a newer one; shutdown and growing-file reads must have bounded behavior.

## 3. Simplify project state and identity

**Priority: before refresh, cancellation or multiple projects.** `Opened` plus `previous: Option<Opened>` mixes current project state with an operation in progress. A retained project disappears from `project()` and the tree during replacement discovery. Discovery discards its thread handle and checks completion against a directory path.

Represent `current: Option<ProjectSession>` separately from `pending: Option<DiscoveryOperation>`. Give operations a generation and cancellation ownership. A successful operation swaps the session; failure leaves it intact. Avoid a second scheduler: extend the existing task infrastructure.

Replace selection strings parsed from tree IDs with a typed selection and stable unit/target IDs. `scope()` currently falls back to project scope for an unrecognized target; reject stale selections instead of silently broadening an action. Tree labels and automation selectors must not define build-tool identity.

Acceptance: current project remains visible through replacement; cancelled/stale completions cannot replace it; invalid target selection cannot become a project-wide action.

## 4. Make commands one registration with a clear action contract

**Priority: before Debug, Coverage and custom actions.** Commands and availability checks live in separate maps and require two registration calls. Projects obtains availability by building a `ProcessSpec`, then builds it again for execution. `StandardAction` is closed to Build/Run/Test; runnable-target policy partly lives in the shared target enum.

Register one command descriptor containing identity, presentation, availability and execution. Represent availability explicitly as enabled or disabled with a reason. Query capabilities without constructing invocations; plan only when executing, validating again against the current selection.

Use provider-owned action IDs with common IDs for standard IDE actions. Keep provider-specific target semantics and options in the provider. Introduce only the configuration types needed by actual actions. Prove the result with a second real build tool before stabilizing the project API.

Acceptance: menu, toolbar, shortcut and programmatic execution agree; one provider can add an action without editing a central enum; selecting a package without a runnable default has a useful explanation.

## 5. Track service and registration ownership

**Priority: before publishing a plugin API.** Services are keyed by Rust type without owner metadata. `ActivationContext::service` and `require_service` return any previously registered service without checking declared dependencies. Their contract is weaker than command/contribution dependency validation.

Record service owners and check access consistently with declared dependencies. Keep registration ownership available after activation, with explicit disposal for owned resources. Restrict raw commit closures to trusted registry implementation details where practical; module-facing APIs should describe the contribution rather than accept an arbitrary side effect.

The current staging primitive is useful, but it is not rollback after a callback panic or a hot-unload protocol. Define resource disposal first; do not implement speculative hot reload now.

Acceptance: undeclared service access fails deterministically; rejected activation leaves nothing registered; shutdown disposes resources in dependency-safe order without hanging.

## 6. Narrow the native view interface

**Priority: before more UI plugins depend on it.** The new extension point removes the closed built-in-view restriction, but `ViewContext` exposes `Context<Workbench>` and custom model/events are untyped JSON. Renderer implementations are coupled to the concrete workbench and manually dispatch events to arbitrary targets. Observation is optional and provides only a subset of interactive state.

Offer a view-scoped event sink and observation API. Keep the native model/event path typed, with type erasure at the registry boundary if needed; use serialization at actual persistence/process boundaries. Avoid a new general schema language. Give observable interactive controls a consistent contract for IDs, focus, enabled state and meaningful values. Preserve the ability to use native GPUI elements.

Acceptance: a plugin view can be tested without depending on workbench internals; mismatched native model/event types are rejected early; keyboard and pointer interaction both reach the owning provider; missing/duplicate observation IDs are detected by tests.

## 7. Make queue and scheduling semantics explicit

**Priority: tighten before more background services.** Retain the bounded inbox, shutdown wakeup and per-turn limits. `post` currently blocks or returns `Full` depending on the thread that created the inbox, and rejection consumes the supplied completion. The workbench uses an extra wake channel plus a 1 ms timer to yield between batches.

Separate explicit nonblocking submission from background backpressure, returning rejected work when retry is meaningful. Separate producer and UI-drain capabilities. Document cancellation and closed-queue handling. Prefer the UI executor's turn scheduling over a fixed delay after verifying its behavior; do not simply delete the yield or replace the queue on stylistic grounds.

Acceptance: a flood remains bounded, input continues between batches, producers unblock on shutdown, and rejected work is either recoverable or explicitly discarded. These are deterministic functional tests; performance measurements remain suspended.

## 8. Separate observation from presentation evidence

**Priority: test credibility.** The current semantic registry observes prepaint, while screenshots capture the X server independently. The earlier native-counter checkpoint visibly captured the preceding frame; the final screenshot was correct.

Give checkpoints a defined render/presentation synchronization contract and correlate image, semantic snapshot and input trace. Use a meaningful renderer/compositor completion mechanism where available; bounded stability checks must be labelled as a weaker fallback. An arbitrary sleep is not proof of presentation.

Acceptance: a deliberately delayed frame cannot produce a successful checkpoint pairing new state with old pixels. Keep dropped/misdirected-input controls and add view-specific keyboard cases.

## Structural cleanup after the contracts settle

- Split the roughly 1,235-line Projects module into session/discovery, action execution/output, source documents and view adapters. Split the roughly 1,470-line workbench into shell layout, view rendering and input/command routing. Preserve public entry points; do not create more crates just to shorten files.
- Group command data and predicates in one owned record instead of parallel maps. Use existing validated ID types consistently and add new ones only for durable domain identity.
- Replace free-form error strings with small domain error enums where callers must distinguish cancellation, unsupported action, invalid configuration and tool failure. Keep human-readable rendering at the UI boundary.
- Evaluate a typed Cargo metadata/message adapter before expanding the manual JSON parser. Keep the pinned UI stack unless an actual dependency problem justifies migration.
- Keep one current status/handoff source; mark earlier findings as historical to avoid competing acceptance claims.

## Recommended execution order

1. Document ownership/navigation and persistence correctness, with failing regression tests first.
2. Project state/identity and the command/action contract.
3. Service ownership and the scoped native-view contract.
4. Queue API and synchronized UI evidence.
5. File-level decomposition, documentation consolidation and a second provider to test the resulting abstractions.

Resizable docking, editing, LSP and Tree-sitter are feature milestones. They should consume these improved contracts; they are not substitutes for this refactoring pass. Continue local checks and leave performance testing suspended. No commits, pushes or ktask execution are authorized in this pass.
