<!-- ABOUTME: Documents the one-time setup for live seren-router functional gates.
ABOUTME: Makes clear that these tests use real processes and never network mocks. -->

# Live functional tests

These ignored tests run the real Rust router, the pinned stock agentgateway binary, and
a real OpenAI-compatible model server against a disposable PostgreSQL 17 database.
Ordinary `cargo test` runs need PostgreSQL for the ledger integration test but do not
need a model server.

Set `DATABASE_URL` to a disposable PostgreSQL 17 database before running either live
walkthrough. The harness applies the repository migrations but deliberately does not
truncate or delete caller-owned rows, so never point it at production.

## LM Studio

Install and start LM Studio, then load a small chat model with a stable API identifier:

```bash
lms server start --port 1234 --bind 127.0.0.1
lms load <model-key> --identifier seren-functional -y
./scripts/fetch-sidecar.sh
DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/seren_router_functional \
SEREN_TEST_MODEL=seren-functional \
  cargo test --test functional -- --ignored --test-threads=1
```

The defaults are:

- `SEREN_TEST_UPSTREAM_URL=http://127.0.0.1:1234/v1`
- `SEREN_TEST_MODEL=gemma-3-1b-it-glm-4.7-flash-heretic-uncensored-thinking_gguf`
- `SEREN_TEST_SIDECAR_BIN=sidecar/bin/agentgateway`

Override the model unless the default model is already loaded under that exact
identifier.

## Ollama

Start Ollama's OpenAI-compatible server, pull a chat model, and point the harness at
its `/v1` endpoint:

```bash
ollama serve
ollama pull <model>
./scripts/fetch-sidecar.sh
DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/seren_router_functional \
SEREN_TEST_UPSTREAM_URL=http://127.0.0.1:11434/v1 \
SEREN_TEST_MODEL=<model> \
  cargo test --test functional -- --ignored --test-threads=1
```

## Lifecycle

Every test allocates its own loopback ports, compiles an isolated test registry, polls
agentgateway's real `GET /healthz/ready` endpoint, and owns all child/task/temp state.
Teardown stops the router and sidecar, waits for the child, removes artifacts, and
verifies that all runtime ports can be rebound. The deliberately dead provider also
uses an OS-assigned unused port.

The routing tests register the same real model server as both a cheap `local` provider
and an expensive `expensive` provider. Their fake registry prices let the harness prove
strict price sorting and the default price ceiling without requiring a second model
process or spending provider credits. A third route points to an unused loopback port
to prove both sidecar first-request retry and the router's pre-commit safety-net retry.

## Paid OpenRouter parity and soak

`tests/functional/parity.rs` is a separate, ignored harness for the M6 compatibility
gate. It runs the checked-in production registry with OpenRouter as its only enabled
provider and compares direct OpenRouter requests with requests through the real pinned
sidecar and router. It never runs in ordinary CI.

Requirements:

- `SEREN_ROUTER_KEY_OPENROUTER` must already be exported from an approved secret source.
  Never put it in a command, test output, repository file, or shell history.
- `DATABASE_URL` must point to a disposable PostgreSQL 17 database.
- `SEREN_PARITY_MAX_SPEND_USD` is mandatory, must be greater than zero, and may not
  exceed `5`. `0.10` is sufficient for the default model and all three gates.
- `SEREN_PARITY_MODEL` is optional and defaults to
  `meta-llama/llama-3.3-70b-instruct`. Any override must be an enabled mapping in
  `registry/providers.yaml`.

Fetch the pinned sidecar and run the small parity checks first:

```bash
./scripts/fetch-sidecar.sh
SEREN_PARITY_MAX_SPEND_USD=0.10 \
  cargo test --test openrouter_parity openrouter_response_parity \
  -- --ignored --nocapture --test-threads=1
SEREN_PARITY_MAX_SPEND_USD=0.10 \
  cargo test --test openrouter_parity openrouter_streaming_parity \
  -- --ignored --nocapture --test-threads=1
```

Only after both parity checks pass, run the 100-request-per-path soak:

```bash
SEREN_PARITY_MAX_SPEND_USD=0.10 \
  cargo test --test openrouter_parity openrouter_streaming_soak \
  -- --ignored --nocapture --test-threads=1
```

Every paid test performs a conservative preflight estimate using 512 input tokens per
call, enforces the caller-supplied budget against observed `usage.cost`, and redacts the
credential from sidecar startup diagnostics. The soak alternates 100 direct and 100
routed one-token streams, requires zero failures, and reports the model, both p95
latencies, nonnegative p95 added latency, both total costs, and combined cost. It fails
when router p95 minus direct p95 is 50 ms or higher.

The registry prices were refreshed from the live `seren-models` catalog on 2026-07-25.
Refresh and review them before rerunning M6 after any OpenRouter price or model change.
