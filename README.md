# RetroWork

Performance testing is suspended until exclusive host access is available. `bash scripts/check.sh` runs all other gates; `cargo run --release -p retrowork-testkit --bin retrowork-perf` remains available for explicitly requested exclusive-host runs with unchanged budgets.

RetroWork is the start of a desktop IDE in Rust with a NetBeans-style
workbench: menus and a toolbar over a docked source tree, document tabs in
the centre, panels at the bottom and a status bar. The workbench is built on
a reusable application platform, and features arrive as plugins.

This repository holds the **foundation** and the first IDE workflow: the
platform, a GPUI workbench, a build-tool independent Projects module with a
Cargo and Ninja providers, log plugins, a declarative and an out-of-process plugin, a
demo application, and the test, coverage and performance gates. See [docs/architecture.md](docs/architecture.md).

Current behavior and acceptance evidence: [status](docs/status.md).
Refactoring decisions: [foundation review](docs/refactoring-review.md).

Licensed under Apache-2.0 ([LICENSE](LICENSE)).

## Requirements

Checked on Fedora 45, x86_64, in a COSMIC Wayland session (see
[Platform support](#platform-support)).

- Rust stable (1.95 or newer; developed with 1.99.1), with `rustfmt` and `clippy`.
- ACL tools (`setfacl`) for metadata-preservation tests, Ninja (used by the second provider and its tests), a C toolchain, `pkg-config`, and development files for libxcb, libxkbcommon,
  libxkbcommon-x11, wayland, freetype and fontconfig.
- A Vulkan driver. A GPU is not required: Mesa lavapipe renders in software.
- For UI and performance tests: a Wayland session with `Xwayland` installed
  (each run starts a private nested X server), or `RETROWORK_E2E_DISPLAY`
  pointing at an X display with XTEST, such as Xvfb.
- For the coverage gate: `rustup component add llvm-tools-preview` and
  `cargo install cargo-llvm-cov --locked`.

`bash scripts/prereqs.sh build` reports exactly what is missing and installs nothing.

Fedora:

```sh
sudo dnf install gcc ninja-build acl pkgconf-pkg-config libxcb-devel libxkbcommon-devel \
    libxkbcommon-x11-devel wayland-devel freetype-devel fontconfig-devel \
    mesa-vulkan-drivers xorg-x11-server-Xwayland
```

## Run the demo

```sh
cargo run
```

This starts the project workbench without sample packages or a separate plugin
build. `cargo run -- --help` lists the options. Startup never searches the
working directory for packages.

To run the optional log and native-view samples explicitly:

```sh
cargo build -p retrowork-plugin-sample
cargo run -- --samples --packages "$PWD/packages"
```

The process sample is found beside the application executable, including
when Cargo uses a custom target directory. Installed packages can be selected
with `--packages /absolute/path` or `RETROWORK_PACKAGES`.

Use File > Import Project to turn a folder into a RetroWork project. Select a folder in the system picker and confirm Import. This creates
`.retrowork/project.toml` without changing build manifests. Cargo and Ninja are
detected automatically; folders without a build tool can also be imported.

`Ctrl+O` (File > Open Project) accepts existing RetroWork configurations. Each
opened project joins the current window. Close Project removes the selected
project from the window without deleting its files. Sources, Tests, Resources,
Libraries, Documentation and Project Files organize logical roots; custom mappings
are supported in the [project configuration](docs/project-workspace.md).
Double-click a file to edit text with line numbers, selection, copying
and keyboard navigation.

Select a package or target under *Build Targets*,
then Build (`F11`), Run (`F6`), Test (`Alt+F6`) or Stop (`Ctrl+F2`) from the
Run menu or toolbar. *Build Output* streams the tool's output with its state;
*Problems* lists compiler errors, and selecting one opens the file at that
line.

For Cargo, opening a project runs only `cargo metadata --no-deps`,
which reads the manifests and does not build or execute project code. Build,
Run and Test run Cargo in the project, which compiles it and executes its
build scripts and procedural macros, and Run and Test also execute the
program and its tests. Use those actions only on projects you trust.

For Ninja, opening runs `ninja -t targets all`. Build uses Ninja's defaults
or the selected target. Run and Test are enabled when `build.ninja` declares
those targets. Run > Clean Ninja Outputs uses `ninja -t clean`.

`fixtures/projects/` holds small projects with only local dependencies used by the tests; the
demo can open them too.

Save with **Ctrl+S** or **File → Save**; undo/redo uses **Ctrl+Z/Ctrl+Y** on Linux.
**Ctrl+Alt+S** or **File → Save All** saves dirty source documents across open
projects without switching tabs. Files already saving finish their current
snapshot; edits made during that save need a later save. Failures retain edits
and identify the affected path in Output. Saves are independent, so a failure
does not stop other documents from saving.
Tabs with unsaved changes show `*`. Close and normal exit ask before discarding
unsaved work. Cancel, Escape and Enter keep the edits; choose Discard explicitly
to close without saving. Saves in progress must finish first. Read-only files remain viewable.
Saves preserve Linux ownership, permissions, ACLs and extended attributes, and
reject detected disk conflicts. **Ctrl+Shift+S** or **File → Save As…** opens the
system save dialog for an editable source. This first version requires a new
file name and refuses existing destinations. It keeps the same tab and undo
history, leaves the original untouched, and sends later saves to the new path.
Crash recovery and confirmed overwrite through Save As are not implemented yet.

**File → Reload from Disk** rereads an editable source without closing its tab.
Unsaved edits require explicit discard confirmation. A successful reload clears
undo history and adopts the disk contents as the saved baseline. Failed reads
or edits made while reading preserve the buffer. This is a manual action;
automatic external-change detection is not implemented yet.

## Check

| Command | What it runs |
|---|---|
| `bash scripts/check.sh` | The full gate: all checks below except suspended performance tests. Fails if any step fails or cannot run. |
| `bash scripts/check.sh --fast` | Formatting, Clippy, rustdoc, all `cargo test` suites. No window needed. |
| `cargo test --workspace` | Unit, integration, headless GPUI and source-rule tests. |
| `cargo run -p retrowork-testkit --bin retrowork-e2e` | The demo executable in a real window, driven by X11 input. |
| `cargo run --release -p retrowork-testkit --bin retrowork-perf` | Manual exclusive-host run; suspended from the default gate. Budgets remain in `perf/budgets.toml`. |
| `bash scripts/coverage.sh` | Line coverage of platform, Projects, Cargo, Ninja and log plugins, at least 90%. |

Before running E2E directly, build its application and sample executable with
`cargo build -p retrowork-demo -p retrowork-plugin-sample`. For an exclusive-host
performance run, build those same packages with `--release` first.

Evidence is written to `target/check.log` (the whole gate run), `target/e2e/`
(screenshots, element snapshots, event traces, `report.json`),
`target/perf/report.json` and `target/coverage/`.
[docs/testing.md](docs/testing.md) explains the test tiers and how to add a
UI test; [docs/quality.md](docs/quality.md) explains the gates and budgets.

## Repository layout

| Path | Contents |
|---|---|
| `crates/platform` | Toolkit-independent kernel: modules, dependency validation, commands, contributions, view models, shell state, persistence, packages, process plugins. |
| `crates/workbench` | GPUI presentation: the workbench window, the semantic element registry and the opt-in automation channel. |
| `plugins/projects` | Bundled module: build-tool independent projects, Build/Run/Test/Stop, Build Output, Problems, editable source documents, and the provider contract. |
| `plugins/cargo` | Bundled module: the Cargo provider (`cargo metadata`, JSON compiler messages). |
| `plugins/ninja` | Bundled module: Ninja discovery, shared actions and a custom Clean action. |
| `plugins/logs` | Bundled module: log sources, records table, filter, record detail. |
| `plugins/log-stats` | Bundled module built only on public APIs: statistics view and summary command. |
| `plugins/external-sample` | Out-of-process plugin executable; shares no code with RetroWork. |
| `packages/` | A declarative package (sample logs, filters, shortcuts) and a process package. |
| `fixtures/projects/` | Test projects: workspaces, local dependencies, build scripts, multiple targets and compile failures. |
| `apps/demo` | The demo application. |
| `crates/testkit` | UI scenarios, headless and native drivers, the E2E and performance runners. |
| `perf/budgets.toml` | Performance budgets and the measurements behind them. |
| `scripts/` | The gate scripts behind the Makefile. |

## Platform support

| Platform | Status |
|---|---|
| Linux x86_64, Wayland session, nested Xwayland for tests | Checked: build, all gates. |
| Linux, X11 session or Xvfb | Expected to work through `RETROWORK_E2E_DISPLAY`; not checked. |
| Linux, native Wayland window | The demo runs as a Wayland client; UI tests do not cover this path. |
| macOS, Windows | Not checked. The platform crate is portable; the test harness is Linux/X11 only. |

## Status and next steps

The foundation is complete when `bash scripts/check.sh` passes; see
[docs/status.md](docs/status.md) for what is proven, what is not, and the
next steps.
