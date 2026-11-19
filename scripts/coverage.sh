#!/usr/bin/env bash
# Line coverage of the toolkit-independent crates: retrowork-platform,
# retrowork-projects, retrowork-cargo, retrowork-ninja, retrowork-logs and retrowork-log-stats,
# measured by their own tests.
#
# Scope: this does not measure the GPUI workbench, the demo executable or the
# test kit; those are covered by the headless and native UI suites instead.
#
# Fails when cargo-llvm-cov is missing or coverage is below the floor.
set -euo pipefail

cd "$(dirname "$0")/.."

# Required minimum, in percent of lines. Change it only by hand, with the
# measurement that justifies the new value.
readonly threshold=90

if ! cargo llvm-cov --version >/dev/null 2>&1; then
    echo "MISSING: cargo-llvm-cov, required for the line-coverage gate" >&2
    echo "         rustup component add llvm-tools-preview && cargo install cargo-llvm-cov --locked" >&2
    exit 1
fi

mkdir -p target/coverage
cargo llvm-cov --locked \
    -p retrowork-platform -p retrowork-logs -p retrowork-log-stats -p retrowork-projects -p retrowork-cargo -p retrowork-ninja \
    --lcov --output-path target/coverage/lcov.info
cargo llvm-cov report --summary-only \
    -p retrowork-platform -p retrowork-logs -p retrowork-log-stats -p retrowork-projects -p retrowork-cargo -p retrowork-ninja \
    --fail-under-lines "$threshold" | tee target/coverage/summary.txt
echo "line coverage of platform, projects, cargo, ninja, logs and log-stats: at least ${threshold}% (details: target/coverage/lcov.info)"
