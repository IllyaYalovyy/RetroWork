# Writing plugins

There are three ways to add a feature. Pick the least powerful that works.

## Declarative package

A directory with a `package.toml`, placed in the packages directory
(`--packages` or `$RETROWORK_PACKAGES`). It contains no code.
It can contribute data to extension points of modules it depends on, and
menu items, toolbar items and shortcuts that run those modules' commands.
Unknown keys are errors.

```toml
id = "sample.workspace"            # dotted lowercase id
name = "Sample Workspace"
version = "0.1.0"                  # semver
kind = "declarative"
requires-api = "^0.1"              # platform API version

[[dependency]]
id = "retrowork.logs"
version = "^0.1"

[[extension]]                      # the Log Explorer's source list
point = "logs.sources"
data = { id = "auth", title = "auth.log", group = "services", path = "data/auth.log" }

[[menu-item]]
menu = "filters"
id = "errors-only"
title = "Errors Only"
command = "logs.filter.set"
args = { query = "level:error" }
order = 10                         # optional

[[toolbar-item]]                   # same keys as a menu item, without `menu`
[[key-binding]]                    # key = "ctrl-1", command, args
```

A package is loaded completely or not at all: if any extension is rejected
(for example a log file that does not exist), none of its contributions
appear, and the Output says why. `packages/sample-workspace` is a complete
example.

Key chords use `ctrl`, `alt` and `shift` with a letter, digit, named key or
`f1` to `f12`. A chord without `ctrl` or `alt` is only accepted for function
keys, so a shortcut cannot swallow typing. The first module to bind a chord
keeps it.

## Process package

`kind = "process"` adds an executable and the commands it implements:

```toml
[process]
command = "retrowork-plugin-sample"   # in this directory, or next to the application
timeout-ms = 5000                     # per request, from submission to reply

[[command]]
id = "sample.top-components.run"
title = "Top Components"
```

The executable is started on first use and speaks **protocol version 0**
(experimental, will change) on stdin and stdout, one JSON object per line:

```text
host   -> {"type":"hello","protocol":0}
plugin -> {"type":"ready","protocol":0,"plugin":"<package id>"}
host   -> {"type":"invoke","request":1,"command":"<command id>","context":{"document":{...}}}
plugin -> {"type":"result","request":1,"effects":[...]}
       or {"type":"error","request":1,"message":"..."}
```

`context.document` describes the active document (`kind`, `resource`,
`title`, `data`); for a log, `data` holds the filter, counts and up to 5,000
of the records currently shown. Effects are `{"type":"output","lines":[...]}`
(at most 500 lines of 4 KiB) and `{"type":"status","text":"..."}`.

Rules the host enforces: a reply must come within the timeout; messages are
at most 1 MiB; anything other than the reply to the current request is a
protocol violation. On a timeout, exit or violation the process is killed and
the user sees the error; the next request starts it again. At most one
request runs and four wait; more are refused. stderr is discarded.

Limits today: plugins run with the user's privileges, cannot add views, and
cannot cancel requests. Child processes a plugin starts are not stopped with
it. `plugins/external-sample` is a complete plugin that shares no code with
RetroWork.

## Project provider (bundled module)

Support for a build tool is a bundled module that depends on
`retrowork.projects` and registers a `ProjectProvider` with the
`ProjectRegistry` service during activation. Cargo and Ninja are working
examples. Providers describe discovery invocations, parse captured output,
declare actions, check availability, plan invocations and interpret output.
They must not start processes, retain session state or touch the UI; Projects
owns execution, cancellation and presentation. Providers are not yet
installable packages.

Each unit and target has a provider-owned ID, separate from its displayed
name and kind. Discovery rejects missing or duplicate IDs. Target sources
are optional: a Ninja output need not have one main source file. Selection
passes these IDs back to the provider; it never infers tool flags from labels.

Declare shared actions using `build`, `run` and `test`. A custom action ID
must start with the provider ID and a dot. Registration creates a command
`projects.action.<action-id>` and a Run-menu item, using the same availability
and task lifecycle as shared actions. Ninja contributes `ninja.clean` this way.
Providers explicitly declare supported actions; the current project's
provider decides availability.

`plan` receives command arguments as JSON at the command boundary. Decode
and validate them into provider-specific types before building argv. Cargo
accepts `release`, `profile`, `workspace`, `features` and `arguments`; unknown
fields and conflicting options fail. Program/test arguments follow `--`.
An explicit workspace action is valid for Build/Test at project scope.
These options are available to command callers and declarative contributions;
there is no configuration editor or saved launch configuration yet.

## Bundled module

A Rust crate implementing `retrowork_platform::Module`, compiled into the
application by adding it to the builder (`apps/demo/src/lib.rs`). It can use
everything in `ActivationContext`: commands, menus, toolbar items,
shortcuts, docked views, document factories, services and extension points.
It describes content with the platform's view models and never depends on
GPUI. `plugins/log-stats` is an example built only on public APIs of the
platform and the Log Explorer.

Bundled modules are part of the application build. They are not installable
by users.

## Native view adapters

For toolkit-independent content, use the built-in view models. For native
GPUI elements, define a `NativeModel` with an associated event type and a
`NativeProvider` that returns the model and handles those events. A
`ViewRenderer` names the same model type. Register it through `ViewRenderers`
from a module that depends on `retrowork.workbench`. Native model/event data
stays in Rust; type erasure occurs only at the registry boundary. Wrong
event types are rejected and renderer lookup checks the concrete type.

`ViewContext` exposes GPUI's window/app and view-scoped helpers, without the
concrete workbench entity. Use `listener` to map pointer or keyboard input to
a typed event; its destination is bound to the owning provider. Use
`focus_handle` for persistent control focus. Declare observable IDs with
`ViewRenderer::controls` and attach `Control` through `observe`, including
value, focus, selection and disabled reason where relevant. Tests reject
missing declared IDs and duplicate observed IDs. Native plugins remain
trusted code: these contracts cannot detect an interactive element a plugin
never declares.

`apps/demo/src/custom_view.rs` demonstrates a counter driven by pointer,
Enter and Space, with actual input tests and frame-matched screenshots. GPUI's focused
`on_click` controls already handle Enter/Space releases; do not add a second
increment in a key-down listener.

## Services, resources and commands

Services retain their module owner. Activation-time lookup rejects access
outside the module, core and declared dependencies. Command handlers,
availability checks, document/view events and worker completions receive
`ServiceAccess`, preserving that same scope after activation. Direct host
access through `Platform::services` remains unrestricted.

Use `Registry<T>` for dependent contributions. Registration is staged,
retains the contributor and is removed on shutdown. Register cancellable
workers or other disposable resources with `ActivationContext::manage`.
`ModuleResource::shutdown` must be short and nonblocking. Resources are
stopped in reverse activation order; failed activation disposes its staged
resources. This is not panic rollback or hot unload.

Workers use an inbox producer. `try_post` returns rejected work immediately
when full or closed. `post_blocking` waits for capacity on workers and always
rejects calls from the creating UI thread. Both return ownership of rejected
completions. Close wakes blocked producers; treat closure as cancellation.
Only the host drains the queue and installs its waker.

Register a `CommandDefinition` through `ActivationContext::register_command`, adding its predicate with `available_when`. Identity, title, handler and availability commit together. The `command` convenience method remains available for commands without a predicate. Checks must be cheap and pure. The workbench shows the reason on unavailable controls, and command dispatch enforces it for shortcuts and queued calls too. Availability checks do not replace validation of arguments inside a handler.

## Logical projects and source documents

Projects owns `.retrowork/project.toml`, Import/Open/Close, and multiple independent
sessions. A provider's `logical_roots(&Project)` suggests categorized relative
paths. Explicit configuration mappings replace suggestions. The shared categories
are Sources, Tests, Resources, Libraries, Documentation, Project Files and Other;
build units and actionable targets remain under Build Targets. Projects enumerates
folders lazily and scopes every tree item and asynchronous result to its project.

`Shell::choose_folder(FolderRequest)` requests the system folder picker; its
confirmation invokes the supplied command with a `directory` argument. Cancelling
invokes no command. The platform request contains no GPUI objects.
`ViewContent::Source(SourceModel)` carries shared text, a one-based reveal line,
and a loading/error message. The workbench owns the read-only editor, cursor,
selection and scrolling, keyed by stable document ID and released on close.
