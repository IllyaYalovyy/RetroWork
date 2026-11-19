# RetroWork — scope of work for a complete Rust IDE

This is the intended product scope, not a list of completed features. Current
implementation and evidence are tracked in [status](status.md). Rust is the
first complete language experience. Keep the platform reusable for other
languages, but defer new Go, Java, Maven and Gradle implementations.

## Product principles

- A modern, performant Rust application using GPUI, with a rich desktop
  workbench that looks and feels close to NetBeans.
- A monorepo with three clear layers: reusable application platform, bundled
  core plugins, and optional/community plugins. Feature logic belongs in plugins.
- Reuse established libraries, components, protocols and native tools before
  writing new code. Inspect relevant open-source IDE implementations, respect
  their licenses, and keep integration code small. Do not build a compiler,
  language server, debugger, Git implementation or search engine ourselves.
- Cargo remains the build system. Execute native tools directly, with visible
  arguments, working directories and environment; no unnecessary command wrappers.
- A useful, complete IDE without AI. AI and agents are optional capabilities
  integrated through the same commands, review tools and extension contracts.
- Testability at every layer. AI must be able to test actual UI interactions
  end to end, including dialogs, editing, debugging and recovery.

## Application platform and plugin infrastructure

- Dependency-aware discovery of capabilities and versioned services supplied
  by other plugins. Optional capabilities must degrade gracefully with reasons.
- Stable identities for projects, documents, commands, views, tasks and results;
  explicit ownership, cancellation, bounded queues and cleanup on closure.
- Common contributions for menus, toolbars, shortcuts, context menus, editor
  gutters, docked views, project nodes, settings, diagnostics and project actions.
- Shared document/edit transactions, filesystem watching, process execution,
  persistent state and credentials services. Plugins must not each invent these.
- Plugin SDK, examples, contract tests, compatibility policy and migration rules.
  Validate manifests and settings before activation; expose failed activation.
- Plugin manager: inspect installed plugins and dependencies, enable/disable,
  install/update/remove optional packages, and recover from failed upgrades.
  Restart-required updates are acceptable; safe hot unloading is not assumed.
- Explicit trust and permissions for external plugins, process execution,
  filesystem access and network use. Trusted native plugins are not sandboxed.
  Evolve the current process protocol before treating it as a community SDK.
- Refactorings, AI skills and prompt-based actions can be contributed by plugins;
  all edits use the shared preview, apply and undo contracts.

## Settings, profiles and themes

- Typed, documented, versioned settings with validation, defaults, migrations and
  useful error locations. Editable configuration files such as TOML remain first class.
- Defined precedence: defaults, user settings, project settings and optional
  machine-local overrides. Show each effective value and where it came from.
  Keep portable project settings separate from personal state and secrets.
- Plugins register their own settings through the same system. Support search,
  reset, safe live changes and a clear restart-required indication.
- Export/import settings and profiles. Support opt-in synchronization, including
  a Git-backed settings repository, with conflict resolution and no secret export.
- NetBeans shortcuts by default; fully configurable context-sensitive bindings,
  conflict detection and profiles for VS Code, IntelliJ, Eclipse and Zed.
- Separate editor and workbench themes. Include system-following light/dark and
  accessible high-contrast themes. Editor colors follow the UI unless overridden.
- Configurable fonts, scaling, tab/indent policy, line endings and file exclusions;
  respect applicable project formatting settings and EditorConfig.

## Workbench and desktop integration

- Resizable, rearrangeable docks; split editors; pinned/preview tabs; sensible
  focus restoration; layout/session persistence and a reset-layout action.
- Multiple projects per window, clear active-project identity, and multiple
  windows without conflicting ownership or lost changes.
- Useful native title-bar content without losing natural window controls or
  desktop behavior. Reuse system file/folder dialogs and existing icon assets.
- Project, package, module, folder, file, generated-source and status icons.
  Icons supplement labels; active projects must remain easy to identify.
- Command search, keyboard access to every action, navigation history, recent
  projects/files, bookmarks, breadcrumbs and a document outline.
- Non-blocking progress and cancellation for long operations. Problems, output,
  tool logs and notifications have stable, discoverable homes.
- Accessibility, focus order, screen-reader support, IME, clipboard, drag/drop,
  HiDPI and multiple-monitor behavior. Validate native Wayland separately from X11.
- Linux is the first validated desktop. Track macOS/Windows support explicitly
  rather than assuming GPUI or a tool adapter makes the entire IDE portable.

## Editor plugin and shared document model

- Real editable buffers with Unicode-safe movement and selections, undo/redo,
  multi-cursor and rectangular selection, indentation, comments, bracket matching,
  folding, snippets, line operations, search/replace and configurable whitespace.
- Safe save/save-as and Save All across projects, dirty-state handling, read-only files, encoding/line-ending
  handling, external-change detection, conflict resolution, autosave options,
  crash recovery and bounded local history.
- The same file opened in normal, split and diff views shares one document and
  undo history. Multi-file edits validate document versions and apply as a
  coherent transaction; failures must not silently leave partial refactorings.
- Incremental Tree-sitter parsing/highlighting with an existing Rust grammar;
  semantic highlighting from the language server where available. Keep syntax
  parsing and semantic analysis separate, and reject stale asynchronous results.
- Plugins can extend editor commands, gutters, side panels and navigation without
  depending on editor internals or replacing the document buffer.
- **No automatic popups.** Completion, signature help, documentation and code
  actions open only on an explicit shortcut or context-menu request.
- **No inserted virtual text or unsolicited inline augmentation.** Put type hints,
  inferred information, blame, test/coverage state and AI suggestions in gutters,
  borders or explicitly opened panels. Syntax/error highlighting is allowed.
  Suggested text becomes actual source only after an explicit edit is accepted.
- Fast handling of large files, long lines and many open documents. Expose limits
  and reduced-functionality modes instead of freezing or silently dropping data.

## Rust language services and refactoring plugins

- Use rust-analyzer through a reusable LSP client. Manage server startup, version
  compatibility, configuration, progress, cancellation, logs, restart and failures.
- Completion and auto-imports, requested signature/documentation help, diagnostics,
  quick fixes, semantic tokens, document/workspace symbols and reference search.
- Go to definition, declaration, implementation and type; navigate traits and
  implementations, callers/callees where supported, and back/forward history.
- Rename across files, import organization and rust-analyzer assists such as
  extraction and transformations where supported. Preview multi-file changes,
  preserve undo and explain when an operation is unavailable.
- First-class macro experience: declarative/procedural macro context, expansion
  viewer, generated-code navigation and mapped diagnostics where tooling supports
  them. Expansion views must not pretend to be ordinary editable source files.
- Keep analysis consistent with the selected toolchain, target, Cargo features,
  workspace members and `cfg` conditions. Explain inactive code and incomplete
  analysis; do not fabricate a semantic model from filenames.
- Build scripts and procedural macros can execute project code. Make execution
  policy explicit and let users inspect what is enabled for a project.
- Treat rust-analyzer capability support as authoritative. Optional/unsupported
  requests must not be presented as working actions.

The reuse baseline is rust-analyzer's documented
[features](https://rust-analyzer.github.io/book/features.html) and
[assists](https://rust-analyzer.github.io/book/assists.html); the IDE supplies the
interaction model, not a second Rust semantic engine.

## Projects, Cargo workspaces and logical navigation

- Import creates `.retrowork/project.toml`; Open accepts an existing project
  configuration. Import must not rewrite Cargo manifests or rearrange sources.
- New-project support for Cargo applications, libraries and workspaces, using
  Cargo tooling and configurable templates. Add/remove workspace members safely.
- Understand workspace roots, members/default members, packages, crates and
  targets: libraries, binaries, tests, examples, benchmarks and build scripts.
- Present tidy logical categories: Sources, Tests, Examples, Benchmarks,
  Generated Sources, Dependencies, Documentation and Project Files. Offer a
  secondary Files view for arbitrary files without making it the project model.
- First-class Rust module navigation within each crate, including inline modules,
  `mod.rs`, `#[path]`, conditional modules and target-dependent structure.
  Reuse language information; a module tree is not a filesystem tree.
- Identify generated sources through provider/build information or explicit
  configuration. Show their origin and regeneration route where known; warn
  about overwritten edits. Keep them navigable and explicitly includable in search.
  Do not classify every ignored file as generated or index all build output.
- Refresh after manifest, lockfile, feature, toolchain or filesystem changes;
  handle renames/deletions, watcher overflow and missed events through reconciliation.
- **Visible project health:** mark discovery, analysis, build or tool failures on
  the project; show the failing component, actionable diagnostics and retry/reload.
  Never leave an apparently healthy IDE that cannot analyze or build its project.
- Restore open projects, active targets, document positions and launch selections
  across restarts, while keeping machine-specific state out of shared settings.

## Toolchains, dependencies and Cargo configuration

- Discover installed Rust toolchains and rustup overrides; honor
  `rust-toolchain.toml`, `.cargo/config.toml`, workspace inheritance, editions and
  minimum supported Rust version. Explain PATH and component mismatches.
- Manage toolchain/target/component installation through explicit user actions;
  offer `rust-src`, rustfmt and Clippy where needed. Preserve offline workflows.
- Dependency browser: direct/transitive and normal/dev/build dependencies,
  enabled features, target-specific edges, path/Git/registry sources and versions.
- Inspect feature resolution, duplicate versions and dependency provenance.
  Navigate dependency source and documentation without treating it as editable
  project-owned code. Understand vendoring, private registries and lockfiles.
- Add/remove/update dependencies using Cargo facilities; preview manifest and
  lockfile changes. Keep credentials in the shared credential service.
- Edit feature selections and build profiles through a consistent configuration
  UI, while leaving Cargo files authoritative and directly editable.
- Rustfmt and Clippy actions, configurable format/check on save, and diagnostics
  routed to the shared Problems view. Background checks must not fight explicit
  builds for Cargo locks or create an unbounded queue of repeated work.

## Project actions, terminal and task plugins

- Standard Build, Check, Clean, Run, Test, Debug and Coverage actions with explicit
  project/package/target scope and availability explanations. Support custom actions.
- Named run/debug/test configurations: binary or test target, profile, features,
  target triple, arguments, working directory, environment and pre-launch tasks.
  Separate shared configurations from local paths and secrets.
- Run/debug the test under the caret; rerun the last action with its original
  project and configuration even after focus changes. Show that context clearly.
- Structured tool diagnostics, bounded output, cancellation of process groups,
  exit status, task history and deliberate concurrency/resource limits.
- Integrated terminal using an established PTY/terminal implementation. Support
  multiple sessions, project working directories, shell selection, ANSI output,
  links to source locations and correct resize/copy/paste behavior.
- Open a shell or external tool with the same toolchain/environment as the IDE
  action. Support Cargo aliases and user-defined commands without a mandatory wrapper.

## Test explorer and coverage plugins

- Discover, filter, run and rerun Rust unit, integration and documentation tests;
  distinguish ignored tests, failures, panics and unsupported discovery modes.
- Tree of suites/tests with status, duration, captured output and source navigation.
  Run a selected test, package, workspace or prior failures; stop running tests.
- Use `cargo test` as the baseline. Offer cargo-nextest as an optional runner;
  keep runner-specific capability differences visible and retain a doctest route.
- Debug a selected test through the same launch/configuration system. Derive test
  executables from Cargo output, not guessed paths under `target`.
- Coverage through an existing tool, initially
  [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov): per-file/line results,
  gutter marks, uncovered-line navigation, aggregate reports and export.
- Label the measured project/package/target/feature scope. Detect stale results
  after edits; distinguish excluded or uninstrumented code from covered code.
  Make unsupported coverage combinations explicit.
- Benchmark discovery/execution and result comparison are separate from tests;
  they must not run automatically on every edit or normal test action.

## Debugger plugin

- Reusable Debug Adapter Protocol client, with a proven LLDB-based adapter for
  Rust. Evaluate adapter/toolchain compatibility instead of implementing a debugger.
- Launch and attach; breakpoints, conditions, hit counts/logpoints where supported,
  step in/over/out, pause/continue, call stacks, threads, variables and watches.
- Rust-aware values through toolchain/debugger formatters; useful handling of
  enums, strings, collections, optimized-out values and missing debug information.
- Debug binaries, examples and tests; source mapping, environment, arguments,
  debug console, session termination and clear launch/attach errors.
- Capability-driven support for memory, disassembly, remote targets and core dumps
  as later extensions. Do not claim every adapter or target supports every action.

[CodeLLDB](https://github.com/vadimcn/codelldb/blob/master/MANUAL.md) is an adapter
candidate, not a commitment to its entire extension or a promise of identical
Rust support across platforms and debug-information formats.

## Search and navigation plugins

- File finding across open projects: fuzzy paths/names, project and crate context,
  useful treatment of recent/open files, duplicate-name disambiguation and full
  keyboard operation. Keep generated/dependency/ignored-file inclusion explicit.
- Reuse Nucleo, ignore and notify as validated by the disposable prototype;
  finish event filtering, asynchronous open validation and performance acceptance
  before integration. See [the library decision](decisions.md#search-libraries).
- Later content search: literal/regex, case/whole-word options, include/exclude
  globs, selection/open-file/project/workspace scopes, cancellation and result navigation.
- Replace across files with preview, checked document versions and coherent undo.
  Search current unsaved buffers as well as disk, without duplicate or stale hits.
- Background indexes with bounded resources, freshness/progress reporting and
  recovery after missed filesystem changes. Select a content-index engine through
  a source-code search prototype; Tantivy remains a candidate, not a final choice.
- Symbol search and semantic references come from language services, not the
  filename matcher or an approximation based on text occurrences.

## Git plugin

- Repositories, nested repositories and worktrees mapped clearly to open projects;
  repository status, changed/untracked/ignored files and useful gutter indicators.
- Stage/unstage files, hunks and selected lines; commit and amend with a reviewable
  staged snapshot. Honor user identity, signing, hooks, attributes and ignore rules.
- Branch/tag navigation, switching, remotes, fetch/pull/push, stash, worktrees,
  cherry-pick, merge and rebase with clear operation state and abort/continue.
- History graph and search, file history, rename-aware navigation and requested
  blame in gutters/panels. Inspect revisions without changing the checkout.
- Conflict resolution and recovery. Make destructive operations explicit and
  preserve unsaved work; failed operations must not leave the UI reporting success.
- Reuse mature Git libraries where suitable and native Git for operations or
  compatibility that require it. Respect credential helpers and SSH configuration.
  No custom Git engine; Git operations remain usable without GitHub or AI.

## Change review and editable diff plugin

This is a first-class workflow for human- and AI-authored changes alike.

- A review workspace with a changed-file tree, cross-file next/previous-change
  navigation, reviewed/unreviewed state, filters and a compact change summary.
- Side-by-side and unified views; selectable bases such as HEAD, index, commit,
  branch merge-base or a captured pre-agent snapshot. Always label the base and
  whether the comparison includes staged, saved or unsaved changes.
- **Edit the working file directly in the diff view and update the diff live.**
  Reuse the ordinary document buffer, undo history, save behavior, highlighting
  and language services. Immutable revision panes stay read-only.
- Preserve caret, selection and scroll while recomputing hunks asynchronously.
  Handle file creation/deletion/rename, line-ending-only changes, large diffs and
  binary files honestly. Whitespace filtering must not alter source text.
- Stage/unstage/revert selected hunks or lines through the Git plugin. Revalidate
  hunk versions before applying; do not apply a stale patch to changed content.
- Three-way merge/conflict editing with explicit completion state, and easy
  navigation between review, source, diagnostics, related tests and history.
- Invalidate reviewed markers when their content changes. Keep review state
  across sessions without treating a previous review as approval of new edits.
- AI changes use the same review workspace; distinguish proposed, applied,
  saved and staged changes, and allow explicit acceptance/rejection and undo.

**Dependency/order:** build a reliable shared editable document model and basic
editor first, then implement editable diffs as another view of those documents.
Complete LSP, debugging and every editor convenience are not prerequisites.
Do not build a second editor or a read-only diff viewer as the final solution.

Reference patterns: IntelliJ's editable working-copy pane and cross-file diff
navigation ([Diff viewer](https://www.jetbrains.com/help/idea/comparing-file-versions.html)),
VS Code's separation of staged and unstaged changes and hunk actions
([staging](https://code.visualstudio.com/docs/sourcecontrol/staging-commits)),
and Neovim's buffer-oriented diff workflow
([diff documentation](https://neovim.io/doc/user/diff/)). These inform the design;
they do not require copying an extension wholesale.

## Optional GitHub integration

Yes: use a separate optional GitHub plugin, built on generic Git, review and
credential services. GitHub must not be required for local Git or change review.

- Account/host configuration, repository links and issue/PR browsing.
- Fetch/open PR branches or worktrees; inspect diffs and review conversations,
  track review state and compose comments/reviews with explicit submission.
- Display existing check results and links. Configuring CI/CD is outside the
  current development phase; viewing remote status is a separate capability.
- Keep provider-specific API behavior outside the core so other forge plugins
  can use the same local review workflow later.

## AI, agents and additional optional plugins

- Provider-neutral AI/agent adapters, optional local models and explicit context
  selection. Expose capabilities and settings without tying the IDE to one vendor.
- Skills/prompts packaged as plugins and actions, with declared inputs, permissions,
  tool requirements and versions. Use ordinary IDE services for edits and actions.
- Visible task activity, cancellation, controlled tool execution and a record of
  changed files. Isolate parallel agent work where needed and route edits through
  review; never hide autonomous file modifications or credential access.
- Documentation: rustdoc navigation/generation, local dependency docs, Markdown
  editing/preview, README links and on-demand documentation panels.
- Smart spelling: comments, documentation and identifiers split by snake_case,
  camelCase and acronyms; configurable dictionaries and exclusions. Do not flag
  arbitrary source tokens as prose.
- Dependency advisories and license/policy checks through tools such as
  cargo-audit/cargo-deny; explicitly requested updates and actionable results.
- Profiling/flamegraphs, benchmark adapters, Miri and fuzzing integrations as
  optional tool adapters, with toolchain/platform availability shown clearly.
- Cargo package/publish assistance with dry-run validation and explicit publish
  action; never publish or modify registry credentials automatically.
- Optional remote/container development, cross-compilation runners, embedded
  targets and WebAssembly workflows. Reuse existing remote/debug/build tooling;
  these are not blockers for the first complete local Rust IDE.
- Useful companion-file support for TOML, Markdown, JSON and build/configuration
  files. Full support for other programming languages follows the Rust milestone.

## Reliability, performance and acceptance

- Every plugin has unit and integration tests, contract/lifecycle tests, headless
  interaction tests where appropriate, and native UI scenarios for its user workflows.
- AI-accessible automation operates real mouse/keyboard controls. Preserve pixels,
  semantic snapshots and event traces; negative controls must detect dropped or
  misdirected input and stale evidence. Direct command calls are not UI tests.
- Use isolated native test displays on shared hosts; validate user-facing native
  dialogs, editor interactions and clipboard behavior as well as command results.
- Test missing tools, unsupported capabilities, large projects, Unicode paths,
  external edits, dirty shutdown/recovery, stale results, plugin failures and
  project closure during background work. Never silently report incomplete work as complete.
- Local acceptance requires formatting, strict Clippy, rustdoc, unit/integration,
  headless/native UI tests, measured coverage and manual code-clarity review.
  The current 90% line-coverage gate measures platform, Projects, Cargo, Ninja,
  logs and log-stats; extend measured scope as production plugins arrive. Do not
  describe this as whole-application coverage.
- Performance tests remain suspended until exclusive host access is available.
  Preserve existing budgets. Later require repeatable optimized-build checks of
  startup, memory, indexing, large views, input latency and plugin overhead, with
  recorded rendering/machine conditions, distributions and failing-budget controls.
- Build and validate locally; do not add CI/CD in this phase. Keep source/API
  documentation clear and self-contained, dependencies deliberate and comments useful.
- Before distribution: reproducible packaging, dependency/license inventory,
  installation/uninstallation, settings migrations, update recovery and an explicit
  supported-platform/tool-version matrix. Make telemetry opt-in if introduced.

## Delivery order and definition of complete

| Stage | Main outcome | Exit evidence |
|---|---|---|
| 1. Editing foundation | Shared editable documents, save/recovery, usable workbench and settings contracts | Real typing, undo, save, external-change and recovery scenarios |
| 2. Rust project experience | Cargo/toolchains, rust-analyzer, logical crate/module navigation, file finding | Open a real workspace, navigate/refactor code and recover from analysis failure |
| 3. Review and source control | Git operations and live editable diffs using the same buffers | Edit, undo, stage and resolve conflicts through the actual UI |
| 4. Execution and feedback | Launch configurations, terminal, tests, coverage and debugger | Run/debug a binary and selected test; navigate failures and uncovered lines |
| 5. Daily-use completeness | Content search/replace, documentation, dependency workflows, polished accessibility and sessions | End-to-end work on a nontrivial multi-crate repository without data loss |
| 6. Release readiness and extensions | Accepted performance, packaging, mature plugin contracts; optional GitHub/AI/advanced tools | Full local gates, measured budgets and supported-platform evidence |

Stages can overlap where dependencies permit. File-finder and library-validation
experiments can continue before the editing foundation is complete. Optional
plugins must not delay a useful standalone Rust IDE.

The core Rust IDE is complete when a developer can create/import a workspace,
configure its toolchain/features, navigate and refactor code, edit and review
changes, build/run/test/debug, inspect coverage, manage dependencies and Git,
and recover their work after failures through a consistent, responsive UI.
All claimed workflows must have actual UI-test evidence and explicit limitations.
