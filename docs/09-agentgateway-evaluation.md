<!-- ABOUTME: Hands-on evaluation of agentgateway as the foundation for seren-router.
ABOUTME: Records verified findings from source inspection and a live no-mocks functional test on 2026-07-24. -->

# 09 — agentgateway Evaluation (DECIDED: build on it)

**Decision (2026-07-24): seren-router is built on [agentgateway](https://github.com/agentgateway/agentgateway)** — a Linux Foundation, Apache-2.0, Rust AI proxy (~4k stars, active; evaluated at v1.4.0-alpha.2, revision `6ab7285`). seren-router becomes a **thin OpenRouter-compatibility, pricing-policy, and cost-accounting layer** on agentgateway's routing core, not a from-scratch proxy.

## What was verified — by reading the Apache-2.0 source

All in `crates/agentgateway/` (the OSS crate; no enterprise/license gating found anywhere in the `llm` module):

- **Providers:** native support for OpenAI, Anthropic, Bedrock, Azure, Vertex, Gemini, Groq, Mistral, Cohere + OpenAI-compatible hosts (Together, Fireworks, DeepInfra, Baseten, Cerebras, xAI…) + `baseUrl` override for any custom endpoint (`types/local.rs` `LocalLLMParams.base_url`).
- **Failover:** `virtualModels` with `routing.failover.targets` and priority tiers (`llm/model_router.rs`, test config `types/local_tests/llm_virtual_model_failover_config.yaml`).
- **Health/eviction:** `health.eviction` per model — consecutive-failure threshold, eviction duration, restore score; default unhealthy = any 5xx or connection failure (`http/health.rs`).
- **Load balancing:** power-of-two-choices selection (`types/loadbalancer.rs`) scored by health/latency/load.
- **Cost/catalog module (docs undersold this):** `llm/cost/` — a `ModelCatalog` with per-provider `Rates`, `Usage`, `Breakdown`, refreshed from the **models.dev** open pricing database (`cost/refresh.rs`), covering openrouter, fireworks, deepinfra, baseten, cerebras, groq, xai, azure, cohere, mistral. This is most of our docs/03 catalog-sync and docs/04 cost-math machinery.
- **Retry:** route-level retry policy (codes + CEL expression) in `http/retry`.

## What was verified — live functional test (no mocks)

Release binary `agentgateway-darwin-arm64` v1.4.0-alpha.2 against a real LM Studio server (real GGUF model, real tokens):

1. **Chat completion through the gateway: PASS.** `POST :4000/v1/chat/completions` with a `local/*` model route (`params.baseUrl` → LM Studio) returned a real completion with correct `usage` accounting.
2. **SSE streaming: PASS.** Chunked deltas, a **final usage chunk** (`stream_options.include_usage`), and `[DONE]` — the exact shape our billing contract needs.
3. **Failover: PASS, with one sharp edge.** With default config, a connection-refused primary returned 503 and did **not** fail over (matches docs: bare failover reacts to 429-with-headers). Adding `health: { eviction: { consecutiveFailures: 1, duration: 60s } }` to the failing target produced correct behavior: request 1 failed and evicted the dead backend; requests 2+ transparently failed over to the priority-1 target and succeeded.

**Config used:** see the evaluation config preserved in this repo's future `examples/` (original at `/tmp/agw-test/config.yaml` during eval).

## Sharp edges to own in our layer

1. **Same-request failover** requires attaching the retry route policy (codes + CEL) so the *first* failing request retries onto the next target — eviction alone only protects subsequent requests. Must be configured and functionally tested in Phase 1.
2. **Eviction is opt-in.** Every provider entry in our registry generation MUST emit a `health.eviction` block. A registry entry without one silently loses failover.
3. **Alpha channel.** v1.4.0 is alpha; pin to a vetted release/commit and track upstream. Apache-2.0 + LF governance = fork-safe if the project turns.

## What remains ours to build (the seren-router layer)

- **OpenRouter-compatible surface:** `usage.cost` (USD) in responses, `GET /generation`, aggregated `/models` with pricing, `provider.sort` / `:nitro` / `:floor` semantics, `reasoning: { effort }` passthrough.
- **Routing policies:** fastest-for-price default (throughput-weighted, price-capped, with smoothing/hysteresis/max-share — docs/02) and the price-weighted `balanced` mode. agentgateway's P2C scores health/latency/load, not price; our policy layer drives it (custom scorer upstream-contributed, or selection logic in our layer).
- **Registry compiler:** our declarative provider registry (docs/03) compiles to agentgateway config (models + virtualModels + health + retry blocks).
- **Cost ledger + reconciliation** (docs/04), building on the OSS `llm/cost` catalog.
- **Gateway auth adaptation** (static bearer key validation).
