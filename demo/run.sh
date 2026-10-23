#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if ! command -v bwrap >/dev/null 2>&1; then
    printf 'error: bubblewrap (bwrap) is required for the production-safe demo\n' >&2
    exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
    printf 'error: cargo is required for the demo fixture verification\n' >&2
    exit 1
fi
if ! command -v rustfmt >/dev/null 2>&1; then
    printf 'error: rustfmt is required for the fixture verification profile\n' >&2
    exit 1
fi

export HARNESS_REQUIRE_BWRAP=1
cargo test --test demo_tests production_safe_demo_tasks -- --ignored --exact --nocapture

latest="$(find target/demo-evidence -mindepth 1 -maxdepth 1 -type d -name 'run-*' -printf '%T@ %p\n' 2>/dev/null | sort -n | tail -1 | cut -d' ' -f2-)"
if [[ -n "${latest}" ]]; then
    printf '\nDemo evidence: %s\n' "${latest}"
    printf 'Run summary:   %s/summary.json\n' "${latest}"
fi
