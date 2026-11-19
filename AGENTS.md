# RetroWork working rules

Build a performant Rust IDE with a rich, familiar desktop workbench. Keep the application platform reusable and features in plugins. Testability is a requirement at every layer.

## Local acceptance

Build and run all checks locally. Do not set up CI/CD in this phase.

The full check must require formatting, strict Clippy, unit and integration tests, headless UI interaction tests, real-window tests with operating-system input, line coverage, and a code-clarity review. Missing prerequisites or failed checks must fail the gate.

Require at least 90% line coverage for the platform, Projects, Cargo and bundled log plugins. Report the measured scope explicitly. Do not imply that this measures the whole application.

Performance tests are suspended until exclusive host access is available. Do not run them during normal checks. Keep `cargo run --release -p retrowork-testkit --bin retrowork-perf` available for explicitly requested exclusive-host runs and preserve all budgets. Once re-enabled, measure startup, memory, large-data processing and views, input responsiveness, and plugin overhead. Use repeatable workloads, an optimized build, explicit budgets, warm-up where appropriate, and recorded machine/rendering conditions. Report distributions rather than one timing. Fail on budget violations. Never automatically relax budgets or replace baselines to hide a regression. Verify that the performance gate rejects a deliberately failing case.

UI tests must operate actual controls with mouse and keyboard input. Keep screenshots, state snapshots and event evidence. Negative controls must detect dropped or misdirected input. Do not treat direct command calls as UI interaction tests.

## Code clarity

Prioritize reuse over new code. Before implementing a capability, inspect the existing framework and established libraries for it. Use system services and framework APIs when available. Add only the smallest integration needed; do not build custom replacements for capabilities they already provide. Simplify existing code and remove redundant abstractions as part of the change. Justify custom implementations with a concrete requirement that existing options cannot meet.

Use simple names, small cohesive functions, explicit ownership and bounded resource use. Prefer straightforward code over speculative abstractions.

Document structures and functions in simple, direct English. Describe purpose, behavior, important invariants, errors and side effects where they matter. Keep documentation self-contained. Do not reference specifications, external documents, design-document paths, milestone labels or task identifiers in source comments or API documentation.

Remove low-value comments: narration of obvious code, redundant headings, implementation diaries, promotional claims and comments that merely repeat names. Keep concise explanations of non-obvious constraints and decisions. Preserve required legal and license notices.

Enforce objective checks with tooling, including rustdoc validation and applicable documentation lints. Review clarity and comment value manually; do not claim a text scanner proves semantic quality. Review existing foundation code as well as new changes.

## Commit messages

Use direct, simple English focused on the impact of the change. Use an imperative subject. Include a body only when it explains a necessary reason or consequence that the subject cannot convey.

No conventional-commit prefixes, emojis, generated summaries, tool attribution, marketing, filler, repeated file lists or unrelated details. Every word must help the reader understand the change.

Examples:
- Keep the UI responsive when a plugin stops reading
- Restore open documents after restart
- Reject changes that exceed performance budgets

Every commit must use Illya Yalovyy <1623643+IllyaYalovyy@users.noreply.github.com> as the sole author. Never add Co-authored-by trailers or bypass hooks.

## Current execution

Codex is implementing the Cargo infrastructure and framework refactorings directly in this pass. Keep ktask configured but idle until the foundation and its quality bar have been reviewed and accepted. Do not create commits or push during this pass.
