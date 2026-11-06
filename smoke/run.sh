#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

export LLM_BASE="${LLM_BASE:-${HARNESS_LLM_BASE:-http://127.0.0.1:8080}}"
export SMOKE_OUTPUT_DIR="${SMOKE_OUTPUT_DIR:-$repo_root/target/local-model-smoke}"
mkdir -p "$SMOKE_OUTPUT_DIR"

echo "local-model smoke: base=$LLM_BASE output=$SMOKE_OUTPUT_DIR"
if [[ -n "${LLM_MODEL:-}" ]]; then
  echo "local-model smoke: model=$LLM_MODEL"
else
  echo "local-model smoke: model will be discovered from GET /v1/models"
fi

cargo test --test local_model_smoke_tests local_model_compatibility_smoke_suite \
  -- --ignored --exact --nocapture

latest="$SMOKE_OUTPUT_DIR/latest.json"
if [[ -f "$latest" ]]; then
  echo "local-model smoke: latest report $latest"
fi
