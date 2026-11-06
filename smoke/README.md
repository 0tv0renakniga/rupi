# Local-model compatibility smoke suite

This suite is run manually against a loopback OpenAI-compatible endpoint such as
`llama-server`. It is not part of ordinary `cargo test` requirements and does not
run in CI.

## Run

Start `llama-server` with the model already loaded, then:

```sh
./smoke/run.sh
```

If the server is not on the default loopback address, or the model ID is not
discoverable from `GET /v1/models`:

```sh
LLM_BASE=http://127.0.0.1:8080 LLM_MODEL=your-model-id ./smoke/run.sh
```

The runner executes the ignored integration test:

```sh
cargo test --test local_model_smoke_tests local_model_compatibility_smoke_suite \
  -- --ignored --exact --nocapture
```

Useful discovery commands:

```sh
curl -s http://127.0.0.1:8080/v1/models | jq -r '.data[].id'
curl -s http://127.0.0.1:8080/props | jq '{build_info, version, default_generation_settings}'
```

## Recorded fields

Each run writes `target/local-model-smoke/latest.json` and a timestamped
`run-*.json` report. The report records:

- model ID and its source;
- llama.cpp / llama-server version and its source;
- configured context cap and reserved output;
- server-reported context size when `GET /props` exposes it;
- time to first visible token, including per-attempt samples and a median;
- empty-response count and rate;
- finish-reason counts;
- cancellation result and cancellation observation details;
- bounded per-attempt outcomes, errors, and discovery warnings.

The report does not store prompts or visible assistant text. Recorded strings are
redacted and truncated, reports are capped at 64 KiB, and report paths are
timestamped so runs can be compared across model or server changes.

## Environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `LLM_BASE` | `HARNESS_LLM_BASE` or `http://127.0.0.1:8080` | Local endpoint base URL |
| `LLM_MODEL` | discovered from `GET /v1/models` | Model ID |
| `LLAMA_SERVER_VERSION` | from `GET /props` | Version override when props is unavailable |
| `HARNESS_CONTEXT_CAP_TOKENS` | `16384` | Recorded effective context cap |
| `HARNESS_RESERVED_OUTPUT_TOKENS` | `4096` | Recorded reserved output budget |
| `SMOKE_ATTEMPTS` | `5` | Normal attempts, clamped to 1-20 |
| `SMOKE_OUTPUT_DIR` | `target/local-model-smoke` | Report directory |
| `SMOKE_REQUEST_TIMEOUT_SECS` | `120` | Per-attempt timeout, clamped to 1-600 |
| `SMOKE_CANCEL_GRACE_SECS` | `15` | Cancel the probe if no visible token arrives, clamped to 1-600 |

## Outcome values

Normal attempts use `completed`, `empty`, or `failed`. Finish reasons are counted
separately, with `transport_error` used when no finish reason was received.

The cancellation probe streams a long response and drops the in-flight request
after the first visible token, or after `SMOKE_CANCEL_GRACE_SECS` if the model
emits only hidden reasoning. Its result is one of:

- `cancelled_after_first_visible_token`;
- `cancelled_before_first_visible_token`;
- `no_visible_token_before_timeout`;
- `completed_before_cancel`;
- `failed`.

## Comparison use

Keep reports with the model/server combination they describe. Compare a new run
against `latest.json` from a prior configuration using fields such as
`llama_cpp_server_version`, `time_to_first_token_ms`, `empty_response_rate`,
`finish_reasons`, and `cancellation_result`. The suite records observations only;
it does not benchmark model quality or modify model/server configuration.
