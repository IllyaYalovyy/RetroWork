# Quality gates

Performance testing is suspended until exclusive host access is available. `bash scripts/check.sh` runs all other gates; `cargo run --release -p retrowork-testkit --bin retrowork-perf` remains available for explicitly requested exclusive-host runs with unchanged budgets.

`bash scripts/check.sh` runs every gate in order and stops at the first failure. There
is no CI; the gate is run locally. A missing tool, display or driver fails
its step with an installation hint.

| Step | Command | Fails when |
|---|---|---|
| Build prerequisites | `scripts/prereqs.sh build` | a compiler, Ninja, `pkg-config` module, rustfmt or Clippy is missing |
| Formatting | `cargo fmt --all -- --check` | any file is not formatted |
| Lints | `cargo clippy --workspace --all-targets -- -D warnings` | any warning |
| API documentation | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | broken intra-doc links or other rustdoc warnings |
| Tests | `cargo test --workspace` | any unit, contract, process-plugin, headless GPUI or source-rule test fails |
| UI prerequisites | `scripts/prereqs.sh ui` | no Xwayland/Wayland session or X display, or no Vulkan driver |
| Native E2E | `retrowork-e2e` | a scenario fails, a negative control is not detected, or a contributed control is not exercised |
| Line coverage | `scripts/coverage.sh` | cargo-llvm-cov is missing, or line coverage is below 90% |
| Performance (manual, suspended) | `retrowork-perf` (release) | a metric is over budget, unmeasured or unbudgeted, or the slow-plugin control is not rejected |

`bash scripts/check.sh --fast` stops after the tests; it is not a substitute for the full
gate. Both keep their whole output in `target/check.log` and
`target/check-fast.log`, and exit with the gate's status.

## Code rules checked by tests

`crates/testkit/tests/source_rules.rs` checks the whole workspace:

- **Documentation of public declarations.** Every source file is parsed with
  `syn`, so qualifiers (`unsafe`, `extern "C"`, `const`, `async`),
  multi-line declarations and attributes are handled as the compiler sees
  them. Required: docs on public functions, structs, enums, unions, traits
  and their items, type aliases, constants, statics, inline and file modules,
  exported macros, public members of inherent `impl` blocks, and every crate
  root. Public items inside private modules are checked too, because they
  may be re-exported. Fixture tests show that undocumented declarations in
  each of these forms are reported and that documented, crate-private and
  test-only ones are not.
- **Self-contained comments.** No comment refers to documents, design
  records, milestones, tickets or task identifiers, and none contains TODO,
  FIXME or XXX. This is a text search over Rust, TOML and shell files.
- **License.** The Apache-2.0 license file and manifest license are present.

Not required: docs on struct fields, enum variants and trait `impl` items,
which are documented only where they say more than their name and type.
Not seen: items generated inside `macro_rules!` bodies.

These checks catch omissions. They do not show that any comment is accurate,
clear or worth keeping; that, and the clarity of crate-private code, is
reviewed by hand.

## Line coverage

Scope: `retrowork-platform`, `retrowork-projects`, `retrowork-cargo`,
`retrowork-ninja`, `retrowork-logs` and `retrowork-log-stats`, measured by their own unit and
integration tests with cargo-llvm-cov. The Cargo tests run real `cargo` on
fixture copies with the coverage tool's variables removed, so fixture builds
are not instrumented. This is **not** whole-application coverage: the
workbench, demo and test kit are not included; the UI suites and control
coverage cover them instead.

Floor: 90% of lines. The current measured result is recorded in
[status](status.md) and `target/coverage/summary.txt`.
The floor is set in `scripts/coverage.sh` and changed only by hand. Use the
per-file report (`target/coverage/summary.txt`) to find untested critical
branches; a higher percentage achieved with assertion-free tests is not an
improvement.

## Performance

`retrowork-perf` refuses to run in a debug build. It uses deterministic
workloads: the sample packages, and a generated 200,000-record log (fixed
seed, about 14 MB). Warm-up runs are discarded. Each metric is reported as a
distribution (count, min, p50, p90, max, mean, every sample) in
`target/perf/report.json`, together with the machine, kernel, compiler,
display, the GPU adapter the window reported (`renderer`), the Vulkan driver
manifests installed (which does not show which one is used), the load
average and the workload parameters.

| Metric | Measures |
|---|---|
| `startup_to_ready_window` | process start until the window exists, has focus, and a prepared frame reports elements |
| `rss_idle`, `rss_large_log`, `peak_rss_large_log` | resident memory after startup, with the large log open, and its peak |
| `large_log_open` | XTEST click in Sources until a prepared frame shows 200,000 records |
| `large_log_filter` | XTEST filter shortcut until a prepared frame shows the filtered table, both directions |
| `click_to_semantic_state`, `key_to_semantic_state` | XTEST click on a tab or a shortcut until a prepared frame shows the change, large log open |
| `observation_round_trip` | one automation snapshot request |
| `log_parse_200k`, `log_filter_200k` | parsing and filtering without a window |
| `platform_build` | resolving and activating every module and package |
| `plugin_cold_start`, `plugin_round_trip` | process plugin request with 5,000 records, cold and warm |
| `project_discovery_40_units` | `cargo metadata` of a generated 40-package workspace and parsing it, no window |
| `project_open_to_tree` | XTEST Enter in the project field until a prepared frame shows all 40 packages |
| `task_output_100k_lines` | the task runner streaming 100,000 lines through the Cargo provider's interpretation, no window |

**What the window timings mean.** Each starts when the X input is sent and
ends when the automation socket, polled every millisecond, reports the
expected state. That state comes from the elements the workbench registers
during GPUI's prepaint, after layout and before painting. The timings
therefore include X server processing, event handling, layout, the polling
interval and at least one observation round trip, and they exclude painting,
GPU work and presentation. They are neither the latency a user sees nor a
bound on it.

**What the budgets are.** Coarse regression baselines for this machine, not
product latency or startup targets. Budgets live in `perf/budgets.toml`,
each with the measurements it was derived from. They were set from three
runs on a shared laptop (load average 7.5 to 17 on 8 CPUs) at about 1.5 to 2
times the worst observed value, so they catch regressions of that size, not
small ones. The runner never edits budgets; change one only by hand with new
evidence in its `basis`. Product targets need presentation-level
measurement, which this gate does not have.

The negative control measures a process plugin that sleeps 300 ms before
every reply against the `plugin_round_trip` budget; the gate fails unless it
is rejected. Unit tests in `crates/testkit/src/perf.rs` check that any
exceeded limit, missing sample, missing budget or unit mismatch fails.

The project metrics were added with the Cargo workflow from three runs on
2026-11-18 (load average 26.5, 9.2 and 7.3); the earlier budgets were not
changed. Real Cargo builds are not timed: their cost belongs to Cargo and
the fixture, not to RetroWork.

Not measured yet: presented-frame latency, frame times while scrolling,
Build Output while a tool floods it in the window, rendering cost on a
software Vulkan driver, cold-cache startup, and anything on macOS or
Windows.
