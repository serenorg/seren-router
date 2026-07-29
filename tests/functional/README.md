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

The sidecar-readiness gate does not need a loaded model. It stops the real pinned
sidecar, requires `/readyz` to return 503 with reason `sidecar`, and confirms
dependency-free `/livez` remains 200.

Two focused gates cover the readiness and beta-isolation changes:

```bash
cargo test --test functional functional_inference_survives_unavailable_database \
  -- --ignored --nocapture --test-threads=1
cargo test --test functional functional_midstream_ledger_loss_recovers_without_interrupting_inference \
  -- --ignored --nocapture --test-threads=1
cargo test --test functional functional_credentials_isolate_production_and_beta_providers \
  -- --ignored --nocapture --test-threads=1
```

The first uses a real unavailable PostgreSQL endpoint and proves that JSON and SSE
inference plus both health endpoints continue serving while the generation-ledger
endpoint fails closed with 503. The second degrades the ledger after SSE response
headers, proves the stream completes, and verifies a later successful write restores
the structured database status. The third runs real production-only and beta-only
provider routes, proves each credential sees only its bound catalog, route,
measurements, and generation records, and proves a forged profile header cannot cross
the boundary.

`functional_streaming_component_latency` is the non-paid regression gate for the
completion hot path. It sends 50 interleaved one-token streams directly to the local
model, through stock AgentGateway, and through the complete router. It reports p95 time
to response headers, first body chunk, and stream completion for all three paths, then
requires the router path to add less than 50 ms at every segment. Run this focused gate
with the same `DATABASE_URL`, `SEREN_TEST_MODEL`, and optional
`SEREN_TEST_UPSTREAM_URL` settings:

```bash
cargo test --test functional functional_streaming_component_latency \
  -- --ignored --nocapture --test-threads=1
```

The routing tests register the same real model server as both a cheap `local` provider
and an expensive `expensive` provider. Their fake registry prices let the harness prove
strict price sorting and the default price ceiling without requiring a second model
process or spending provider credits. A third route points to an unused loopback port
to prove both sidecar first-request retry and the router's pre-commit safety-net retry.

## Paid OpenRouter parity and soak

`tests/functional/parity.rs` is a separate, ignored harness for the M6 compatibility
gate. It runs the checked-in production registry with OpenRouter as its only enabled
provider and compares direct OpenRouter requests with requests through the real pinned
sidecar and router. Generated sidecar routes use AgentGateway's `passthrough: detect`
mode with an explicit upstream-model override, avoiding a redundant response
translation while preserving provider selection and attribution. IPv6 resolution is
disabled by default because the runtime addresses and validated deployment path are
IPv4. The paid tests never run in ordinary CI.

Requirements:

- `SEREN_ROUTER_KEY_OPENROUTER` must already be exported from an approved secret source.
  Never put it in a command, test output, repository file, or shell history.
- `DATABASE_URL` must point to a disposable PostgreSQL 17 database.
- `SEREN_PARITY_MAX_SPEND_USD` is mandatory, must be greater than zero, and may not
  exceed `5`. `0.10` is sufficient for the default model and all three gates.
- `SEREN_PARITY_MODEL` is optional and defaults to
  `meta-llama/llama-3.3-70b-instruct`. Any override must be an enabled mapping in
  `registry/providers.yaml`.
- Both paths are pinned to OpenRouter endpoint `nebius/fp8`, with OpenRouter fallbacks
  disabled. Its reviewed input/output prices match the default model mapping, keeping
  schema and `usage.cost` comparisons independent of OpenRouter's dynamic provider
  load balancing. Refresh the pin and registry prices together.

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
credential from sidecar startup diagnostics. Schema comparisons ignore only keys whose
value is JSON `null`, because optional nullable OpenAI fields may be omitted by a
compatible intermediary; any non-null field loss still fails. The soak interleaves 100
direct and 100 routed one-token streams, alternating which path runs first in each pair
to balance upstream time drift while keeping every call sequential. It requires zero
failures and reports the model, pinned upstream provider, both raw p95 latencies, both
p95 client/network overheads, nonnegative p95 added latency, both total costs, and
combined cost. After all streams finish, the harness uses each preserved
`X-Generation-Id` to read OpenRouter's authenticated per-generation metadata and
subtract its reported `generation_time` from that exact client observation. This
prevents volatile provider inference time from being misclassified as local router
overhead. It fails when routed overhead p95 minus direct overhead p95 is 50 ms or
higher; the unnormalized raw p95 difference remains in the report for auditability.

The registry prices were refreshed from the live `seren-models` catalog on 2026-07-25.
Refresh and review them before rerunning M6 after any OpenRouter price or model change.

## Paid Modal Kimi K3 beta contract

`tests/modal_contract.rs` is a separate, ignored activation gate for the checked-in,
disabled Kimi K3 beta candidate. It enables only the internal `modal` row in its
private fixture, starts the real pinned AgentGateway and router, and uses a disposable
PostgreSQL 17 schema. It never runs in ordinary tests or CI and does not deploy
anything.

Requirements:

- authenticate against Seren's existing Modal workspace and verify the current
  promotional balance plus any visible expiry, restrictions, and Shared API
  eligibility in the account;
- export a short-lived, dot-joined proxy credential as
  `SEREN_ROUTER_KEY_MODAL` from an approved secret boundary;
- record whether Modal workspace RBAC actually enforces environment scope; without
  RBAC, the credential is workspace-wide and must be treated accordingly;
- set `DATABASE_URL` to a disposable PostgreSQL 17 database;
- set `SEREN_MODAL_MAX_SPEND_USD` to a positive decimal no greater than `5`; and
- set `SEREN_MODAL_BILLING_REVIEW_ID` to a fresh UUID created only after reviewing
  current Modal billing; the gate durably consumes it so a failed or timed-out run
  cannot be repeated without another billing review;
- set `SEREN_MODAL_BILLING_STATE_DIR` to an absolute, durable, operator-private
  directory outside both the repository and the system temporary directory. Every
  run records its worst-case reservation there before provider access, and all
  retained reservations count against the cumulative $5 approval; and
- fetch the pinned sidecar with `./scripts/fetch-sidecar.sh`.

Run the gate serially:

```bash
SEREN_MODAL_MAX_SPEND_USD=0.10 \
SEREN_MODAL_BILLING_REVIEW_ID=<fresh-uuid-after-billing-review> \
SEREN_MODAL_BILLING_STATE_DIR=<durable-private-state-dir> \
  cargo test --test modal_contract modal::modal_kimi_beta_contract \
  -- --ignored --exact --nocapture --test-threads=1
```

The harness must print `running 1 test`; abort if it reports zero tests.
The gate preflights each request against a conservative uncached-token estimate,
reserving an initial call plus both configured AgentGateway retries for each of its two
logical requests. All six possible provider attempts remain charged against the local
hard cap even when the request succeeds, because failed or timed-out attempts may not
return usage. The durable reservation is not released automatically; reconcile it
against account billing before changing or retiring the state directory. It sends the
same deterministic prompt through JSON Chat Completions twice and requires the repeat
to report a positive exact cache-hit count. Streaming is excluded: the first bounded
account run showed that Modal completed an SSE request but omitted the terminal usage
object, so the registry makes `stream: true` ineligible for Modal and routes it to a
compatible beta provider. No automatic third probe is allowed; review account billing
and use a fresh review UUID before rerunning a failed gate.
Both responses must preserve the reviewed customer sell subtotal and neutral public
model/provider naming. A cold response without cache detail must persist provider cost
as null; a response with exact detail must persist the exact gross provider cost.
The test also proves the database retains the internal provider ID while
`/generation` returns only the neutral alias.

Tool calling and strict structured output are documented Modal capabilities but remain
manual probes until deterministic fixtures and activation criteria are approved.
Cross-provider failover remains covered by the non-paid functional harness; this paid
gate intentionally isolates Modal so its spend and attribution cannot be confused with
OpenRouter traffic. Its report distinguishes resolved provider cost from the reserved
gross-cost ceiling; account credit consumption is reconciled separately from the
authenticated Credits and billing views. Delete the short-lived proxy token after the
gate unless an approved beta deployment secret has been created separately.
