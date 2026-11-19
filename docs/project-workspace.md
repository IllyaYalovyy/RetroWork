# Project workspace

## Next requirements

These requirements were requested on 2026-11-18 and are not implemented yet.

- Generated sources are first-class project content. Providers should identify
  their origin and owning unit, show a separate Generated Sources category, and
  warn that edits can be overwritten. Keep them navigable for diagnostics and
  offer explicit inclusion in search. Do not equate every ignored file with
  generated code, or infer generation solely from a directory name.
- Package/module navigation should present the language structure compactly,
  with project and build-unit ownership visible. Rust modules can be inline,
  conditionally compiled or mapped to other files; Go packages and modules are
  different concepts. Reuse language tooling instead of inventing a parser or
  treating directory hierarchy as authoritative. Keep ordinary files accessible
  when language information is unavailable.
- Tree rows need consistent icons from the existing UI library. Project roots
  and the active project should be visually distinct from folders, packages,
  source files and build targets. Icons must supplement text labels.
- The next plugin is file finding only, across open projects. Provide fuzzy
  matching, project-relative paths, clear ownership for duplicate names and
  mouse/keyboard navigation. Respect ignore rules and bound indexing work;
  stale candidates must not open files belonging to a closed project. Reuse
  Nucleo and established traversal/watching libraries. Content search follows
  separately; see the search-library evaluation in [decisions](decisions.md).

Acceptance needs real UI interaction evidence for filtering and opening results,
including duplicate names, ignored/generated content policy, rename/deletion,
project closure and dropped input. Performance gates remain suspended until
exclusive host access is available.

## User workflow

File > Import Project opens the system folder picker. Import identifies the build tool
and creates
`.retrowork/project.toml` only after the user confirms Import. It never rewrites
the build tool's manifest or moves project files. A failed discovery or cancellation before publication leaves no configuration
behind. Once atomic publication begins, Stop reports that import is finishing;
it cannot report cancellation after successfully creating configuration. Existing configurations are never
overwritten by import.

File > Open Project uses the system folder picker and accepts only a directory containing
a valid `.retrowork/project.toml`. An unconfigured folder explains that Import
Project is required. Opening never silently imports. GPUI's `prompt_for_paths` delegates folder selection to the operating system
(desktop file chooser portal on Linux). RetroWork implements only the asynchronous
result handler. Headless tests simulate platform cancellation; native tests operate
the installed system chooser with OS input on an isolated test display.


Opening adds a project to the current window. Opening its canonical directory
again selects the existing project. Closing a project removes its workspace
membership without deleting files or configuration. Commands use the selected
project and its selected unit/target; selection in one project cannot redirect
another project's running task or diagnostics.

## Configuration and ownership

`.retrowork/project.toml` is portable project metadata intended for version control.
It contains a schema version, display name, provider ID and optional logical-root
mappings. Paths are relative to the configuration's project directory. Unknown
schema versions fail with an explanation and remain untouched. Invalid paths,
duplicate IDs and malformed data fail before the project becomes active.

Build manifests remain authoritative for packages, targets, dependencies and
build behavior. Configuration customizes organization; it does not duplicate the
Cargo, Ninja or future Maven/Gradle/Go models. User-local window layout and open documents belong to application session state,
not the shared project file. Open project membership is currently window-local
and is not restored across restarts. Import uses a non-overwriting atomic
publication step after successful validation and discovery.

The platform remains independent of project formats. Projects owns configuration,
workspace membership, logical navigation and action selection. Providers own
build-tool discovery, semantics and suggested logical roots. The workbench requests the system picker and routes results back to the requesting plugin.
A filesystem browser must not become special Cargo logic inside the workbench.

## Logical project structure

Each open project is a tree root. Multi-module projects show their modules beneath
that root. Within a project or module, use these shared categories when relevant:

| Group | Contents |
|---|---|
| Sources | Production source roots and their files |
| Tests | Test roots and test targets |
| Resources | Non-code runtime assets and configuration |
| Libraries | Dependencies reported by the provider; distinguish workspace and external dependencies |
| Documentation | Documentation roots and relevant project documents |
| Build Targets | Executables, examples, benchmarks and other actionable tool targets |
| Project Files | Build manifests and IDE configuration |
| Other | Explicitly mapped files or roots that do not fit another group |

Providers map their conventions into these categories. Empty categories are
omitted; absent dependency information must not be presented as a complete empty
dependency list. Source directories can retain useful subdirectories beneath their
logical roots, but the project view is not a raw dump of the project directory.
Generated outputs and tool caches are excluded unless explicitly mapped. Tree
IDs identify project, unit and item independently of their displayed labels.
File enumeration is lazy and bounded; results for closed projects are discarded,
while an operating-system read already in progress cannot be interrupted. no recursive scan runs during
rendering or on the interaction thread. Symlink handling must prevent cycles and
unintended traversal outside configured roots.

## Text viewer

Opening a source or documentation file creates or selects a document tab. The
source editor supports editing, undo/redo and background Save: monospaced text, line-number gutter,
current-line indication, selectable/copyable text, horizontal and vertical
scrolling, keyboard navigation and line/column status. Diagnostic navigation
selects and reveals the requested location. It must not follow the bottom of the
file like a streaming Output panel. Closing and reopening releases the old buffer
and restores the intended navigation location. Editing and saving are separate
features; the UI must not imply that typed changes are saved.

## Implementation and acceptance

1. Introduce versioned configuration and import/open services with atomic import,
   bounded I/O and tests for cancellation, malformed files and existing metadata.
2. Replace the single current project with stable project sessions and independent
   selection/output/diagnostic state. Preserve identity in queued completions.
   Keep the existing one-running-task admission rule explicit for this version.
3. Use the system folder picker for separate Import/Open/Close commands.
4. Add provider logical-root contributions and lazy grouped navigation.
5. Add a distinct source-viewer model and renderer rather than extending log output
   behavior with project-specific branches.
6. Exercise actual pointer and keyboard flows for import, cancelled import,
   rejecting unconfigured Open, opening two projects, switching action targets,
   closing/reopening, grouped file navigation and viewer selection/copy/navigation.
   Keep screenshots, matching snapshots and traces, plus dropped/misdirected-input
   negative controls. Run the full local gate and report coverage scope explicitly.

Performance benchmarks stay suspended. No CI, ktask execution, commits or pushes
are part of this work.

## Configuration example and current limits

```toml
version = 1
name = "My application"
provider = "cargo"

[[roots]]
group = "sources"
path = "src"

[[roots]]
group = "tests"
path = "tests"

[[roots]]
group = "documentation"
path = "docs"

[[roots]]
group = "project-files"
path = "Cargo.toml"
```

Omit `roots` to use the provider's suggested roots. Explicit roots replace those
suggestions. Names use `sources`, `tests`, `resources`, `libraries`,
`documentation`, `project-files` or `other`; paths must stay inside the project.
Missing paths are omitted. Configuration is limited to 64 KiB and 128 explicit
roots. A folder without a build tool uses `provider = "projects.files"`; its
standard source/test/documentation directories are recognized, and extra files
can be mapped explicitly. It has no build actions.

Providers can override `ProjectProvider::logical_roots`. The current Cargo and
Ninja defaults recognize common directories and manifests; resolved external
library/dependency browsing is not implemented. Cargo workspace members must be
imported through their workspace root. There is no configuration editor or import
preview wizard yet; edit the TOML and close/reopen the project to apply changes.

Project navigation rejects directories above 2,000 entries, admits at most four pending
reads per project, and stops admitting reads once 20,000 paths are cached. Pending
reads may add up to 8,000 further paths. Symlink children are omitted. Source files
use the existing bounded UTF-8 loader. The viewer cannot edit or save.

## Reuse and review findings

GPUI already provides both system folder selection and a native editor. The
workbench delegates to those facilities; custom directory enumeration belongs
only in logical project navigation. The reusable platform carries a small picker
request and a source-view model, while Projects owns import/open validation.

Project identity must follow asynchronous output and navigation results. Switching
projects cannot retarget a running task; invalid selections are checked before
changing the active project. Closing a document also drops its workbench editor
state. Import has an explicit publication boundary so cancellation cannot be
reported after metadata has already been committed.
