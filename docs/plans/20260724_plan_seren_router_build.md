<!-- ABOUTME: Complete zero-context implementation plan for building seren-router.
ABOUTME: Bite-sized tasks with files, code sketches, exact test commands, and commit points. -->

# seren-router Build Plan — 2026-07-24

This is the complete implementation plan for seren-router. It assumes you are a skilled
developer who has **never seen this codebase, Seren, agentgateway, or OpenRouter**. Read it
top to bottom once before writing any code. Every task tells you what to build, which files
to touch, how to test it, and when to commit.

---

## Part 1 — Context (read this even if you're impatient)

### What you are building, in three sentences

Seren Desktop is an AI chat app. All of its public AI-model traffic flows through one
billing proxy ("the Gateway", `api.serendb.com`) to a publisher called `seren-models`,
which today is just a dumb forward to **OpenRouter** (openrouter.ai) — a commercial
aggregator that routes one OpenAI-style API across ~70 inference providers. You are
building **seren-router**: the service that replaces OpenRouter, so Seren calls inference
providers (Together, Fireworks, Blackbox, DeepInfra…) directly with its own API keys,
keeps the middleman margin, and controls its own routing.

### The one constraint that shapes everything

The Gateway is configured with exactly one upstream URL for `seren-models`:
`https://openrouter.ai/api/v1`, plus a stored API key, and it reads the billed cost from
the JSON path `usage.cost` in every response. **Cutover day is a one-field change**: point
that URL at seren-router instead. Therefore seren-router MUST be wire-compatible with
OpenRouter — same endpoints, same request/response shapes, same `usage.cost` field. If we
get the wire format right, nothing else in Seren changes.

### What we are NOT building (YAGNI — reread this list whenever you feel creative)

- NO customer-facing billing, credits, or wallets. The Gateway does that. We only report
  true upstream cost in `usage.cost`.
- NO user accounts, NO multi-tenancy. One caller: the Gateway, with one static key.
- NO UI. NO Kubernetes controller. Flat config files.
- NO hand-written streaming proxy, provider adapters, failover, or load balancing —
  **agentgateway** (below) provides all of that. If you find yourself writing a
  connection pool or an SSE parser for upstream traffic, stop: you are duplicating the
  sidecar.
- NO speculative abstractions. Two similar lines are fine; a trait with one impl is not.

### The foundation: agentgateway (decided — do not relitigate)

[agentgateway](https://github.com/agentgateway/agentgateway) is a Linux Foundation
open-source AI proxy, written in Rust, Apache-2.0. We verified (see `docs/09`):

- It exposes an **OpenAI-compatible** `/v1/chat/completions` on a local port and forwards
  to 20+ providers, including any OpenAI-compatible host via `params.baseUrl`.
- It does priority-tier failover (`virtualModels` + `routing.failover.targets`), health
  eviction (`health.eviction`), retries, and load balancing.
- **Sharp edge we hit in a live test:** failover does NOT happen on connection failure
  unless you configure `health: { eviction: ... }` on the target. Every generated
  provider entry must include it. Same-request retry needs the retry route policy.
- Verified version: **v1.4.0-alpha.2**, git revision `6ab7285`. Pin exactly this until
  M0-4 says otherwise.

### Architecture (decided — the "why" is in docs/01–09)

```
Gateway (api.serendb.com)
   │  static bearer key, OpenRouter-shaped requests
   ▼
seren-router  (this repo: Rust/axum service — "the layer")
   │  • validates the key
   │  • parses model slug + sort preference (:nitro/:floor/provider.sort)
   │  • PICKS the concrete target (routing policy lives HERE, not in the sidecar)
   │  • rewrites the request to that target's route name
   │  • streams the response back, injecting usage.cost
   │  • records every request in the Postgres ledger (serves GET /generation)
   ▼
agentgateway  (pinned stock binary, localhost sidecar — "the sidecar")
   │  • provider adapters, TLS, retries, health eviction, failover safety net
   ▼
Providers (OpenRouter first, then Together, Fireworks, Blackbox, …)
```

Locked decisions you build against:

| # | Decision | One-line why |
|---|----------|--------------|
| D1 | Sidecar, not library/fork: run the stock pinned agentgateway binary; our code is a separate front service | zero fork maintenance; upgrade = bump a pinned version |
| D2 | Routing policy lives in the layer; the sidecar's failover is a safety net | the policy is our moat; the sidecar's balancer doesn't know price |
| D3 | Postgres for the per-request ledger (`/generation`, reconciliation) | boring, queryable, ubiquitous |
| D4 | Gateway↔router auth = one static bearer key from env, constant-time compare | matches how the Gateway already talks to OpenRouter |
| D5 | Throughput measurements in-memory (EWMA), lost on restart | cold-start is acceptable; persistence is YAGNI until proven otherwise |
| D6 | Service chassis + deployment = [`serenorg/seren-template-rust`](https://github.com/serenorg/seren-template-rust) (Christian's call) | seren-router deploys like every other Seren Rust service: the template's middleware stack, Dockerfile, and k8s health probes, not bespoke scaffolding |

### Reading list (skim now, consult while working)

| What | Where | Why you care |
|------|-------|--------------|
| Design docs 00–09 | this repo `docs/` | strategy, routing policy, billing contract, migration |
| agentgateway source | clone `github.com/agentgateway/agentgateway` at `6ab7285` | ground truth for config schema |
| — config examples | `examples/llm-basic/config.yaml`, `examples/llm-ollama-postgres/config.yaml` | working config shapes |
| — config schema | `schema/config.json` | every legal field |
| — LLM config structs | `crates/agentgateway/src/types/local.rs` (search `LocalLLMParams`) | e.g. `baseUrl` lives under `params`, not the model root |
| — failover test fixture | `crates/agentgateway/src/types/local_tests/llm_virtual_model_failover_config.yaml` | canonical failover config |
| — health/eviction | `crates/agentgateway/src/http/health.rs` (`LocalHealthPolicy`, `LocalEviction`) | eviction fields |
| OpenRouter API reference | `openrouter.ai/docs/api-reference` | the wire format we must match |
| OpenRouter provider routing | `openrouter.ai/docs/features/provider-routing` | `provider.sort`, `:nitro`, `:floor` semantics |
| models.dev | `models.dev` | open model/pricing catalog (agentgateway's cost module syncs from it) |
| Working eval config | this repo `examples/eval-lmstudio-failover.yaml` | a config we proved works, byte-for-byte |
| seren-template-rust | `github.com/serenorg/seren-template-rust` (private) | the service chassis: read its README fully before M0 — middleware stack, feature flags, route registries, Dockerfile |
| — server infrastructure | its `src/server.rs`, `src/routes.rs`, `src/db.rs` | what you get for free; don't rebuild any of it |

---

## Part 2 — Toolset primer

You need: `rustup` (stable toolchain), `git`, `curl`, Docker (only for local Postgres),
and for functional tests either **LM Studio** (`lms` CLI) or **Ollama** as a real local
OpenAI-compatible model server.

Conventions for this repo — follow them exactly:

- **Format/lint:** `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings`
  must pass before every commit. No exceptions, no `#[allow]` without a comment saying why.
- **Commits:** small and frequent — one per task below, more if a task has natural
  seams. Conventional-commit style (`feat:`, `fix:`, `test:`, `docs:`, `chore:`).
  Every commit message body ends with:

  ```
  Taariq Lewis, SerenAI, Paloma, and Volume at https://serendb.com
  Email: hello@serendb.com
  ```

- **Never** commit a secret, an API key, or a `.env` file. Keys enter only via
  environment variables named in the registry (`docs/03`).
- **File headers:** every source file starts with a 2-line `// ABOUTME:` comment
  describing what the file does.
- **Naming:** names say WHAT, never HOW or WHEN (`catalog.rs`, not `new_catalog_v2.rs`).

## Part 3 — How we test (read carefully; this is where taste is enforced)

Two kinds of tests, and the difference is load-bearing:

**1. Unit tests** — for pure logic only: policy math, slug parsing, config generation,
cost arithmetic. Rules:

- Deterministic. No network, no sleeps, no wall-clock time (inject time as a parameter).
- Test the **behavior/contract**, not the implementation. If a refactor that preserves
  behavior breaks the test, the test was wrong.
- **Non-duplicative:** each behavior is asserted in exactly one place. Use table-driven
  tests for input families; use **golden files** (checked-in expected outputs under
  `tests/golden/`) for wire formats and generated configs. When behavior changes
  intentionally, regenerate goldens in the same commit and eyeball the diff.
- Do not test the sidecar's internals. agentgateway's failover logic is upstream's
  code; we test that **our generated config produces the behavior**, once, functionally.

**2. Functional gates** — for anything involving process boundaries, streaming, or
failure handling. House rule, non-negotiable: **NO MOCKS**. A mocked HTTP server proves
your mock works. Every functional gate runs the REAL pinned agentgateway binary against a
REAL upstream:

- A real local model server (LM Studio/Ollama) for success paths.
- A **dead TCP port** for failure paths — a connection-refused from `127.0.0.1:59999` is
  a real failure, not a simulation. This exact pattern proved the eviction sharp-edge in
  `docs/09`.
- Assert on observed wire output (SSE chunks, JSON bodies, status codes), not on logs.
- Test output must be pristine — a passing run prints no warnings, no stray output.

The functional harness you will build in M2-3 is the backbone of every later milestone.
Milestones end with a **Gate**: a functional test that must pass before you move on.

---

## Part 4 — The plan

Milestones M0–M7. Tasks are numbered `M<milestone>-<task>`. Each is sized for roughly one
commit. Do them in order; later tasks assume earlier files exist.

---

### M0 — Repo scaffolding

**Goal:** a cargo workspace that builds, lints, tests, and can fetch the pinned sidecar.

#### M0-1: Instantiate from seren-template-rust (D6 — do NOT `cargo init`)

Clone `serenorg/seren-template-rust`, copy its contents into this service's source tree
(preserving `.github/`, `Dockerfile`, `Makefile`, `.env.example`, `migrations/`,
`src/`), rename the crate to `seren-router` in `Cargo.toml`, and keep the template's
module layout: `server.rs` (middleware stack — the README says don't modify unless
extending; believe it), `routes.rs` (route registries — register ALL our routes through
its `public_router`/`protected_router` builders, never a parallel route list), `db.rs`
(sqlx pool helpers), `auth.rs` (Seren auth stubs — see M2-1 for what we use and don't).

The template requires **Rust 1.95+, edition 2024** and already carries axum 0.8, tokio,
tower-http, sqlx 0.9 (postgres, rustls), reqwest 0.13 (rustls), serde, uuid, jiff,
thiserror, tracing, dotenvy. Add only what seren-router needs on top:
`serde_yaml`, `rust_decimal`, `subtle` (constant-time compare), `rand`, `futures`,
`bytes`, and `reqwest`'s `stream` feature. Use `jiff` for time (template convention) —
do not add `chrono`.

Three template defaults that are WRONG for this service — change them consciously, with
a comment at each site, in this task:

1. **30s request timeout.** LLM streams routinely run for minutes. Exempt (or raise to
   ≥10 min on) the `/api/v1/chat/completions` and `/api/v1/completions` routes; keep the
   default everywhere else.
2. **`payload-limit` feature (1 MiB default).** Vision requests carry base64 images and
   blow through 1 MiB. If the feature is enabled, set the chat routes' limit to ≥20 MiB
   (the desktop's own frontend buffer limit is 20 MiB).
3. **`rate-limiting` (per-IP) — leave OFF.** This service has exactly one caller (the
   Gateway) from effectively one address; per-IP limiting adds risk, not safety.

Feature flags: build with the template's `production` group (metrics, security-headers,
sensitive-headers, payload-limit per note 2).

Test: `make check` if the template's Makefile provides it, else
`cargo build && cargo fmt --all --check && cargo clippy --all-targets -- -D warnings`;
then `cargo run` and `curl localhost:8000/readyz` → `{"status":"ok"}`.

Commit: `chore: instantiate service from seren-template-rust`

#### M0-2: Pinned sidecar fetch script

The sidecar is NOT vendored in git. A script fetches the pinned release binary and
verifies its checksum.

Files:

```
scripts/fetch-sidecar.sh
sidecar/PINNED_VERSION        # two lines: "v1.4.0-alpha.2" and the sha256 of the binary
```

Script behavior: download
`https://github.com/agentgateway/agentgateway/releases/download/<PINNED_VERSION>/agentgateway-<os>-<arch>`
for darwin-arm64 / linux-amd64 / linux-arm64, `chmod +x`, verify sha256 against
`PINNED_VERSION`, install to `sidecar/bin/agentgateway`. Fail loudly on checksum
mismatch. (Get the correct sha256 by downloading once and recording it — the release
publishes `.sha256` files alongside each binary; use those.)

Test (manual, then automated in CI):
`./scripts/fetch-sidecar.sh && ./sidecar/bin/agentgateway --version` → JSON containing
`"1.4.0-alpha.2"`.

Commit: `chore: pinned agentgateway sidecar fetch with checksum verification`

#### M0-3: CI

The template ships `.github/workflows/ci.yml` — **extend it, don't replace it**: add the
fetch-sidecar + `--version` smoke step and a `services: postgres` container (used from
M3 on; harmless before). Keep the template's existing fmt/clippy/test jobs as-is.
Functional gates that need a local model server are tagged `#[ignore]` and run only
where noted (see M2-3).

Commit: `ci: sidecar smoke and postgres service on template workflow`

**M0 Definition of done:** fresh clone → `./scripts/fetch-sidecar.sh && cargo test` works
on a clean machine following only README instructions.

---

### M1 — Provider registry and the config compiler

**Goal:** our declarative registry (`docs/03`) compiles to a valid agentgateway config.
This is pure logic — unit + golden tested, no processes.

#### M1-1: Registry types

File: `src/registry.rs`

```rust
// ABOUTME: Declarative provider registry: which inference hosts exist, how to auth,
// ABOUTME: and which models they serve. Compiles to agentgateway sidecar config.

#[derive(Debug, Deserialize)]
pub struct Registry {
    pub providers: Vec<Provider>,
}

#[derive(Debug, Deserialize)]
pub struct Provider {
    pub id: String,               // "fireworks"
    pub display_name: String,
    pub base_url: String,         // "https://api.fireworks.ai/inference/v1"
    pub secret_env: String,       // "SEREN_ROUTER_KEY_FIREWORKS" — env var NAME, never a key
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub priority: u8,             // failover tier; 0 = preferred, higher = later. OpenRouter fallback = 255
    pub models: Vec<ModelMapping>,
}

#[derive(Debug, Deserialize)]
pub struct ModelMapping {
    pub slug: String,             // canonical "meta-llama/llama-3.3-70b-instruct"
    pub provider_model_id: String,// what THIS host calls it
    pub input_price_per_mtok: rust_decimal::Decimal,   // USD
    pub output_price_per_mtok: rust_decimal::Decimal,  // USD
}
```

Also create `registry/providers.yaml` with one real entry (OpenRouter as priority-255
fallback, per docs/05) and one placeholder (fireworks, `enabled: false`).

Unit tests (same file, `#[cfg(test)]`): YAML round-trip of a two-provider registry;
unknown field rejection (`#[serde(deny_unknown_fields)]` — add it); duplicate provider id
rejected by a `Registry::validate()` you write.

Commit: `feat: provider registry types and validation`

#### M1-2: Config compiler

File: `src/sidecar_config.rs`

Compiles `Registry` → agentgateway YAML. Requirements (each traces to a verified fact in
`docs/09` / the eval config `examples/eval-lmstudio-failover.yaml`):

- Per enabled provider, per model: a route named `"<provider.id>/<slug>"` with
  `provider: openAI`, `params.baseUrl`, `params.model = provider_model_id`,
  `params.apiKey` referencing the env var (agentgateway supports `$ENV_NAME` in
  `apiKey` — see `examples/llm-basic/config.yaml`).
- **EVERY route gets** `health: { eviction: { consecutiveFailures: 1, duration: 60s } }`.
  This is the docs/09 sharp edge #2 — a target without it silently loses failover. Make
  it impossible to emit a route without it (build it in the constructor, not at call sites — DRY).
- Per canonical slug served by ≥2 providers: a `virtualModels` entry named by the bare
  slug with `routing.failover.targets` ordered by provider `priority`.
- `llm.port` from our config (default 4000). `config.readinessAddr` on 19001.

The compiler's job ends at YAML bytes. Do NOT shell out from here (separation: M2 owns
processes).

Tests — golden file: `tests/golden/sidecar_config_basic.yaml` — compile a fixture
registry (two providers, one shared slug, one exclusive slug) and compare byte-for-byte.
One golden covers the whole shape; individual unit tests only for the tricky bits
(priority ordering, eviction always present, env-var passthrough). Don't re-assert the
whole YAML in five tests — that's duplication.

Validation test (functional, cheap, no model server needed):
compile the fixture → write to temp file → run
`sidecar/bin/agentgateway -f <tmp> --validate-only` → assert exit 0 and stdout contains
`Configuration is valid!`. This catches schema drift against the REAL binary — the whole
point of pinning. Mark `#[ignore = "needs sidecar binary"]`; CI runs it after fetch-sidecar.

Commit: `feat: registry-to-sidecar config compiler with mandatory eviction`

**M1 Gate:** golden test green + real `--validate-only` green in CI.

---

### M2 — The layer: proxy skeleton with streaming

**Goal:** an axum service that authenticates the Gateway, forwards chat completions to
the sidecar, and streams responses back unmodified. Cost injection comes in M3 — do not
start it here.

#### M2-1: Service configuration + auth

Files:

```
src/config.rs        # RouterConfig: sidecar_url, gateway_key (from env SEREN_ROUTER_GATEWAY_KEY), registry path
src/gateway_auth.rs  # axum middleware: Authorization: Bearer <key>, subtle::ConstantTimeEq compare, 401 otherwise
```

(Listen address, database URL, and dotenv loading come from the template's existing
config/`db.rs` — don't duplicate them.)

Register our routes through the template's `protected_router` builder with THIS
middleware as the protection. **Note on the template's `auth.rs`:** its
`SerenIdentity`/passthrough-token helpers are for publishers configured with
`auth_type="passthrough"`. The `seren-models` publisher is `auth_type="static"` — the
Gateway presents one static bearer key, exactly as it does to OpenRouter today, and
cutover compatibility requires we keep that (docs/05). So for cutover we use our static
key middleware, NOT the passthrough helpers. Leave the template's `auth.rs` in place;
adopting identity-token verification on top is a post-cutover hardening, tracked in M7's
open items — do not build it now (YAGNI).

Auth rules: missing header → 401 `{"error":{"message":"unauthorized"}}`; wrong key →
401; never log the presented key (the template's `sensitive-headers` feature redacts
`Authorization` from traces — keep it enabled).

Unit tests: middleware with correct/missing/wrong key using `axum::body` +
`tower::ServiceExt::oneshot` (this is in-process request construction against our own
router — that's a unit test of our handler, not a mock of an external system; the
distinction matters).

Commit: `feat: service config and constant-time gateway auth`

#### M2-2: Chat completions passthrough with SSE streaming

File: `src/proxy.rs`

`POST /api/v1/chat/completions`: read the JSON body, forward to
`{sidecar_url}/v1/chat/completions` with `reqwest`, stream the response body back
**chunk-by-chunk as received** (`reqwest::Response::bytes_stream()` →
`axum::body::Body::from_stream`). Copy status and `content-type`. Do not buffer, do not
parse SSE yet (M3 does), do not retry (the sidecar owns retries).

Also: `POST /api/v1/completions` — same passthrough (legacy endpoint; OpenRouter has it,
the Gateway's endpoint list has it — one shared function, two routes. DRY).

`main.rs` now wires config → auth middleware → routes → serve.

Commit: `feat: streaming passthrough proxy for chat and legacy completions`

#### M2-3: The functional harness (the most important file in the repo)

File: `tests/functional/harness.rs` (plus `tests/functional/README.md` documenting the
one-time local setup).

The harness (used by every later gate):

1. Compiles a test registry → sidecar config pointing at a **real local model server**
   (env `SEREN_TEST_UPSTREAM_URL`, default `http://127.0.0.1:1234/v1` = LM Studio;
   document `lms server start` and Ollama alternative in the README) and at a **dead
   port** (`127.0.0.1:59999`).
2. Spawns the real sidecar binary with that config (`std::process::Command`), waits for
   readiness by polling `127.0.0.1:19001` (the readinessAddr — no sleeps).
3. Spawns our router (in-process `tokio::spawn` of the axum app) pointed at the sidecar.
4. Kills both on drop (`Drop` impl — tests must not leak processes; verify with a
   repeated-run test locally).

Gate tests (all `#[ignore = "functional"]`; run with `cargo test -- --ignored` on a
machine with a model server):

- `functional_chat_completion`: POST a chat request with the gateway key → 200, body has
  `choices[0].message.content` non-empty and `usage.prompt_tokens > 0`.
- `functional_streaming`: `"stream": true, "stream_options": {"include_usage": true}` →
  response is SSE; collect chunks; assert deltas arrive, a final chunk contains `usage`,
  and the stream ends with `data: [DONE]`. (We observed exactly this shape in docs/09.)
- `functional_auth_rejected`: no key → 401, and the sidecar saw no request.
- `functional_failover`: request a virtual-model slug whose priority-0 target is the dead
  port → the first request succeeds via the healthy target. The M5-4 retry route policy
  and the router's pre-commit bare-slug safety net both enforce this contract.

Commit: `test: functional harness with real sidecar, model server, and dead-port failover`

**M2 Gate:** all four functional tests green locally. Record the run output in the PR
description.

---

### M3 — Cost accounting and the ledger (the money path — most care here)

**Goal:** every response carries accurate `usage.cost` (USD, provider true cost), and
every request is recorded and retrievable via `GET /api/v1/generation?id=`.
This is the contract the Gateway bills on (`upstream_cost_response_path: usage.cost`).

#### M3-1: Price table

File: `src/pricing.rs`

`PriceTable::from_registry(&Registry)` → lookup keyed by `(provider_id, canonical_slug)`
returning the per-mtoken Decimal prices. Cost function:

```rust
pub fn cost_usd(prices: &ModelPrices, usage: &Usage) -> Decimal {
    // (prompt_tokens * input_price + completion_tokens * output_price) / 1_000_000
}
```

ALL money math is `rust_decimal::Decimal`. If you type `f64` anywhere near a price,
delete it. Unit tests: table-driven — zero tokens, exact-million boundary, rounding
(assert exact Decimal strings, e.g. `"0.0001234500"`), and a hand-computed real example
(document the arithmetic in the test).

Commit: `feat: decimal price table and cost computation`

#### M3-2: usage.cost injection (non-streaming)

In `proxy.rs`: for non-streaming responses, parse the sidecar's JSON, read
`usage.prompt_tokens`/`completion_tokens`, look up the price for the route that served it
(the layer chose the route in M2/M5, so it knows provider + slug), insert
`usage.cost` (JSON number, USD), and return the modified body. Do not add a default
top-level provider field: the current OpenRouter success schema exposes routing details
only when the client opts into `openrouter_metadata`; `provider_name` remains
error-metadata behavior and is not part of this success-path milestone.

Golden test: fixture sidecar response JSON (captured from a real M2-3 run, checked in) →
expected output JSON with cost. One golden for the happy path; unit tests for: missing
usage (pass through unchanged, log warning), unknown model (cost omitted, warning —
NEVER fail the user's request over a bookkeeping miss).

Commit: `feat: inject usage.cost into non-streaming responses`

#### M3-3: usage.cost injection (streaming)

The hard one. Wrap the SSE byte stream in a transformer
(`src/sse.rs`) that: passes chunks through untouched, watches for the
final usage-bearing chunk (`"usage":{...}` with empty `choices` — the shape we observed),
rewrites that one chunk to include `usage.cost`, then passes `data: [DONE]`.

Implementation notes for someone new to SSE: events are `data: <json>\n\n`; a chunk
boundary from `bytes_stream()` does NOT align with event boundaries — buffer partial
lines. Keep the transformer a pure `fn(state, bytes) -> (state, bytes)` core so it's
unit-testable without any I/O.

Unit tests: feed the transformer a REAL captured SSE session (fixture from M2-3, checked
in) split at pathological boundaries — mid-line, mid-JSON, one-byte-at-a-time (loop over
split points; one table-driven test, not thirty copies). Assert output equals input
except the usage chunk gains `cost`.

Functional: extend `functional_streaming` to assert `usage.cost` is present and equals
the M3-1 computation for the observed token counts. (Local models are priced 0 in the
test registry — use a nonzero fake price in the test registry so the assertion is real.)

Commit: `feat: SSE transformer injecting usage.cost into the final usage chunk`

#### M3-4: Postgres ledger + /generation

Files:

```
migrations/0001_generations.sql
src/ledger.rs
```

```sql
CREATE TABLE generations (
  id TEXT PRIMARY KEY,              -- the response id from the provider
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  canonical_slug TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  prompt_tokens BIGINT,
  completion_tokens BIGINT,
  cost_usd NUMERIC(18, 10),
  latency_ms BIGINT NOT NULL,
  status SMALLINT NOT NULL          -- HTTP status returned to the caller
);
```

Write one row per request (streaming: after `[DONE]`; use `tokio::spawn` so recording
never blocks the response — but LOG loudly on failure).
`GET /api/v1/generation?id=` → `{ "data": { id, model, total_cost, tokens_prompt, tokens_completion, ... } }`
matching OpenRouter's generation endpoint field names (check the API reference; the
Gateway's endpoint list includes it).

Tests: sqlx tests against the CI Postgres (`DATABASE_URL`) — insert + fetch round-trip;
unknown id → 404. Functional: after `functional_chat_completion`, hit `/generation` with
the returned id and assert cost matches the response's `usage.cost` (one source of truth
— the ledger row is written from the same computation; the test guards the plumbing).

Commit: `feat: postgres generations ledger and /generation endpoint`

**M3 Gate:** functional suite green including cost assertions; `usage.cost` for a
hand-priced request matches a calculator to 10 decimal places.

---

### M4 — Catalog: GET /models

**Goal:** the endpoint the desktop's model search will eventually use (do NOT touch
seren-desktop — its repoint is gated on cutover, see docs/08 Phase 5 and issue
seren-desktop#3291).

#### M4-1: Catalog assembly from the registry

File: `src/catalog.rs`

`GET /api/v1/models` → `{ "data": [ { id, name, context_length, pricing: { prompt, completion }, ... } ] }`
— match OpenRouter's `/models` field names (the desktop already parses `id`, `name`,
`context_length`; capture one page of the real publisher `/models` output as the golden
reference for shape). Source: the registry (canonical slugs, prices) — a slug appears
once even if three providers serve it, with the cheapest price shown. Add
`context_length` and display `name` as registry fields on `ModelMapping` (M1 amendment —
migration is one commit touching registry.rs, providers.yaml, and the goldens).

YAGNI note: no live sync from provider `/models` endpoints yet. The registry is
hand-curated until the OpenRouter-fallback share shrinks (docs/03 explains why curation
is unavoidable anyway). Unmapped-model discovery is Phase-4+ work — not in this plan.

Golden test: fixture registry → expected `/models` JSON. Functional: harness asserts the
endpoint returns the test registry's slugs.

Commit: `feat: aggregated /models catalog from the registry`

#### M4-2: Compatibility stubs

`GET /api/v1/auth/key` and `GET /api/v1/credits` → minimal valid JSON
(`{"data":{"label":"seren-router","limit":null}}` / `{"data":{"total_credits":0,"total_usage":0}}`)
so Gateway-side probes never 404. `GET /api/v1/models/:model/endpoints` → providers
serving that slug, from the registry. Ten lines each; one unit test each.

Commit: `feat: auth/key, credits, and model endpoints compatibility stubs`

---

### M5 — Routing policy (the moat — docs/02 is the spec, keep it open while coding)

**Goal:** the layer picks the concrete `provider/slug` route per request per policy;
the sidecar executes.

#### M5-1: Preference parsing

File: `src/policy/preference.rs`

From the request: model suffix `:nitro` → Throughput, `:floor` → Price (strip the suffix
before route lookup); body `provider.sort` = `"price" | "throughput" | "latency"`
overrides; neither → `Default`. Also pass through `reasoning: { effort }` untouched
(OpenRouter field the desktop already sends — verify it forwards intact in the M2
passthrough test, one assertion).

Table-driven unit tests: suffix, body, both (body wins — document that choice in the
test name), neither, unknown sort value → 400 with a clear error.

Commit: `feat: routing preference parsing (:nitro, :floor, provider.sort)`

#### M5-2: Measurements

File: `src/policy/measurements.rs`

Per `(provider_id, slug)`: EWMA of tokens/sec (completion_tokens ÷ stream duration) and
time-to-first-token, updated after each request from data the proxy already has. Injected
clock (`fn now(&self) -> Instant` on a trait or pass timestamps in) — no `Instant::now()`
inside logic, or the unit tests can't exist. In-memory `RwLock<HashMap>` (D5). Alpha =
0.2 (constant with a comment; tuning is ops, not code).

Unit tests: EWMA convergence on a fixed sequence (assert exact values — it's
deterministic math); unseen pair → None.

Commit: `feat: EWMA throughput and latency measurements`

#### M5-3: The selector

File: `src/policy/select.rs`

One pure function — this is the heart of docs/02, implement it exactly:

```rust
pub fn select_route(
    candidates: &[Candidate],   // providers serving the slug: price, priority, ewma_throughput, healthy
    pref: Preference,
    cfg: &PolicyConfig,          // price_ceiling, hysteresis_pct, max_share, rng seed source
    recent_share: &ShareTracker, // rolling per-provider share of this slug's traffic
    rng: &mut impl rand::Rng,    // injected — determinism in tests
) -> Option<&Candidate>
```

- `Default` (fastest-for-price): drop unhealthy; drop over `price_ceiling`; weight by
  smoothed throughput; apply hysteresis (a candidate must beat the incumbent by
  `hysteresis_pct` to take share); cap any provider at `max_share` of recent traffic for
  the slug; weighted-random pick.
- `Balanced`: OpenRouter's algorithm — healthy, then weight by 1/price².
- `Price` / `Throughput` / `Latency`: strict sort, no balancing — return the top healthy
  candidate (sequential fallback comes free from the sidecar's virtual-model tiers).
- No candidates → None → proxy returns 404 model-not-found (same error shape as
  OpenRouter's unknown-model error).

Unit tests (the most valuable in the repo — seeded RNG, zero I/O):
- Distribution test: 10k draws, cheapest-of-three under `Balanced` gets ~inverse-square
  share (assert within ±3% — statistical, seeded, still deterministic).
- Price ceiling excludes a fast-but-expensive candidate under `Default`.
- Max-share cap: with one dominant candidate, its share tops out at the cap.
- Hysteresis: a 1% faster challenger does NOT flip the incumbent; a 30% faster one does.
- Each `sort` mode returns the strict best.
- Do NOT test "returns Some" fifteen ways — the distribution tests subsume the happy path.

Commit: `feat: route selector — fastest-for-price default, balanced, and sort modes`

#### M5-4: Wire it into the proxy + same-request retry config

Proxy flow becomes: parse preference → candidates from registry+measurements → select →
rewrite request `model` to the chosen `"provider/slug"` route → forward. On upstream
failure of the chosen route, fall back to the bare-slug virtual model (sidecar's
priority tiers — our safety net, D2).

Also: attach the sidecar's retry route policy so the FIRST request survives a dead
target (docs/09 sharp edge #1). The retry policy shape is in agentgateway's
`schema/config.json` (search `"retry"`; codes + attempts). **Verify at implementation**
which config block it attaches to in local (file) mode — the schema places it under
route policies; confirm with `--validate-only` and then prove it with the functional
test. When it works, tighten `functional_failover` to require the FIRST request to
succeed.

Functional: `functional_sort_modes` — two live local upstreams (two model-server
processes, or one server registered twice with different fake prices), assert
`sort: price` always hits the cheap one (check the ledger's `provider_id`), and Default
respects the ceiling. This is the one place a second local upstream is worth the setup
cost; document it in the harness README.

Commit: `feat: policy-driven route selection with sidecar failover safety net`

**M5 Gate:** distribution unit tests + `functional_failover` (first-request success) +
`functional_sort_modes` all green.

Verified locally on 2026-07-25 with the pinned stock sidecar, a real LM Studio model,
and disposable PostgreSQL 17.

---

### M6 — OpenRouter passthrough parity — complete

**Goal:** prove seren-router-with-OpenRouter-as-only-provider is indistinguishable from
direct OpenRouter (docs/08 Phase 1 gate). Everything here needs
`SEREN_ROUTER_KEY_OPENROUTER` — a real spend, get sign-off on a small budget (<$5).

- M6-1: enable the OpenRouter registry entry (`base_url: https://openrouter.ai/api/v1`,
  priority 255, real model slugs for the top Seren models from `docs/00`).
- M6-2: parity harness (`tests/functional/parity.rs`): same prompt → seren-router vs
  direct OpenRouter; assert response schema equality (keys, not content), streaming
  shape equality, and `usage.cost` within 1% of OpenRouter's reported cost for the same
  generation (they price identically; the delta allowance is for token-count drift).
  Run manually, record output in the PR.
- M6-3: soak: 100 sequential streamed requests per path, zero failures, p95 added
  latency vs direct < 50ms. Normalize each observation with OpenRouter's authenticated
  per-generation timing metadata so upstream inference variance is not counted as
  local router overhead; retain raw p95s in the evidence.

Commit(s): `feat: openrouter fallback provider entry`, `test: openrouter parity and soak harness`

**M6 Gate = Phase 1 gate:** parity + soak green. After this, deploy/canary/cutover
(docs/08 Phases 2–3) are ops runbooks executed with Taariq — infra target and provider
keys are his open items; do not improvise them.

Completed live on 2026-07-25 with the approved OpenRouter credential, pinned
`nebius/fp8` endpoint, stock AgentGateway v1.4.0-alpha.2, and disposable PostgreSQL 17.
JSON and SSE schema/provider/cost parity passed. The 100-direct + 100-routed streaming
soak completed with zero failures, exact aggregate cost parity (`$0.00028700` per
path), and 44.329 ms normalized p95 added latency (113.808 ms direct overhead vs
158.136 ms routed overhead), below the 50 ms gate. Raw p95s (3,223.309 ms direct and
4,111.064 ms routed) remain in the recorded output to show upstream variance.

---

### M7 — Deployment (strategy decided: seren-template-rust conventions; specifics gated)

The deployment strategy is D6: seren-router ships exactly like every other Seren Rust
service built on the template — its multi-stage `Dockerfile` (already includes a health
check), `/livez`/`/readyz` probes, JSON logging in k8s, graceful SIGTERM shutdown, and
`/metrics` (enable the `metrics` feature in the production build).

What is specific to this service:

- **Pod layout: two containers.** The app container (template Dockerfile) and the
  agentgateway sidecar as a SECOND container in the same pod, sharing localhost.
  Verify at implementation whether the project publishes an official container image
  for the pinned version; if not, build a thin image that copies the checksummed binary
  from `scripts/fetch-sidecar.sh` onto a distroless base. The sidecar config is rendered
  by our M1 compiler at startup (init container or entrypoint step) from the registry
  file mounted via ConfigMap.
- **Readiness must be composite.** The app's `/readyz` should also probe the sidecar's
  readiness address (`127.0.0.1:19001`) — a pod whose sidecar is down must not receive
  Gateway traffic.
- **Secrets:** provider keys (`SEREN_ROUTER_KEY_*`) and `SEREN_ROUTER_GATEWAY_KEY` enter
  as k8s Secrets → env vars, matching the registry's `secret_env` names. Never in the
  ConfigMap, never in the image.
- **Streaming vs. ingress:** whatever ingress/load balancer fronts the pod must allow
  long-lived streaming responses (≥10 min idle timeout on the chat routes) — same trap
  as the template's 30s timeout (M0-1 note 1), one layer up.

Still gated on Taariq/Christian — do not improvise: which cluster/environment, DNS name,
Postgres provisioning, secrets-manager binding, and the canary mechanics (docs/08
Phase 2). Also deliberately NOT in this plan: the Gateway cutover itself (one
`update_publisher` call, done WITH Taariq per docs/05), direct-provider onboarding
beyond registry mechanics (blocked on keys + ToS vetting), live catalog sync, the
seren-desktop client changes (gated: docs/08 Phase 5, seren-desktop#3291), and
identity-token auth hardening (M2-1 note). When you think you need any of these early,
reread docs/05 and stop.

---

## Part 5 — Working agreements recap (pin this)

1. Read `docs/02` before M5 and `docs/04` before M3 — they are the spec; this plan is
   the task list.
2. One task ≈ one commit. Push at least daily. Never rewrite pushed history.
3. `cargo fmt` + `clippy -D warnings` + unit tests green before every commit; functional
   gates green before a milestone PR.
4. Money is `Decimal`. Time is injected. RNG is seeded in tests.
5. NO MOCKS in functional gates. Dead ports and real binaries are the tools.
6. Every generated provider route carries `health.eviction`. No exceptions.
7. When the sidecar surprises you, check `schema/config.json` and the structs in
   `crates/agentgateway/src/types/local.rs` at the pinned revision before guessing.
8. Secrets are env-var names in configs, never values. If a key touches git history,
   rotation is YOUR first action, then tell Taariq.
