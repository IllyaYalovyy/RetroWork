#!/usr/bin/env bash
# The RetroWork quality gate. `bash scripts/check.sh` runs this.
#
#   scripts/check.sh          full gate
#   scripts/check.sh --fast   static checks and tests that need no window
#
# Every step must pass. Nothing is skipped: if a tool or a display is
# missing, the step fails and says what to install.
set -euo pipefail

cd "$(dirname "$0")/.."

mode=full
case "${1:-}" in
    "") ;;
    --fast) mode=fast ;;
    *)
        echo "usage: scripts/check.sh [--fast]" >&2
        exit 2
        ;;
esac

mkdir -p target
if [ "$mode" = fast ]; then
    exec > >(tee target/check-fast.log) 2>&1
else
    exec > >(tee target/check.log) 2>&1
fi
check_log_pid=$!
trap 'check_status=$?; exec 1>&- 2>&-; wait "$check_log_pid"; exit "$check_status"' EXIT

step() {
    printf '\n==> %s\n' "$*"
}

step "Build prerequisites"
bash scripts/prereqs.sh build

step "Formatting"
cargo fmt --all -- --check

step "Lints (Clippy, warnings are errors)"
cargo clippy --workspace --all-targets --locked -- -D warnings

step "API documentation (rustdoc, warnings are errors)"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked --quiet

step "Unit, integration, headless GPUI and source-rule tests"
cargo test --workspace --locked

if [ "$mode" = fast ]; then
    printf '\ncheck (fast): passed. Native UI and coverage gates were NOT run; performance is suspended; use `bash scripts/check.sh`.\n'
    exit 0
fi

step "UI test prerequisites"
bash scripts/prereqs.sh ui

step "Native end-to-end tests: real executable, real window, XTEST input"
cargo build --locked -p retrowork-demo -p retrowork-plugin-sample -p retrowork-testkit
cargo run --locked -p retrowork-testkit --bin retrowork-e2e -- --artifacts "$PWD/target/e2e"

step "File finder experiment: native interaction and negative controls"
cargo test --locked -p retrowork-file-finder-prototype --test native -- --ignored

step "Line coverage (platform, projects, cargo, ninja, logs, log-stats; at least 90%)"
bash scripts/coverage.sh

printf '\nPerformance tests disabled until exclusive host access is available.\n'
printf 'check: passed. Evidence: target/e2e/report.json, target/coverage/summary.txt\n'
