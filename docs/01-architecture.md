<!-- ABOUTME: The compatibility seam and internal component architecture of seren-router.
ABOUTME: Describes the OpenRouter-compatible API surface and the five internal parts. -->

# 01 — Architecture

## The seam: a drop-in OpenRouter-compatible service

The entire Seren stack already speaks OpenRouter's dialect end to end:

- The desktop client fetches its model catalog from `openrouter.ai/api/v1/models` (`src/services/models.ts:23`).
- Model IDs use OpenRouter's `vendor/model` slug convention (`anthropic/claude-opus-4.6`, `moonshotai/kimi-k2.5`, `meta-llama/llama-3.3-70b-instruct`).
- The Rust chat worker sends OpenRouter's `reasoning: { effort }` request field (`src-tauri/src/orchestrator/chat_model_worker.rs:612`).
- The Gateway reads cost from `usage.cost` (`upstream_cost_response_path` on the `seren-models` publisher).

That shared dialect is the seam. `seren-router` is built to be **API-compatible with OpenRouter** so the Gateway can point at it unchanged. The cutover is then a one-field change: repoint `seren-models.api_url` from `https://openrouter.ai/api/v1` to `https://<seren-router>/api/v1` and swap the static key. Because the contract is identical, the billing envelope, the 5% fee, the desktop client, and the model picker all keep working with no changes.

## External API surface (OpenRouter-compatible)

seren-router exposes:

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/api/v1/chat/completions` | OpenAI-compatible chat completions; streaming (SSE), function-calling, vision |
| POST | `/api/v1/completions` | Legacy text completion |
| GET | `/api/v1/models` | Model catalog with pricing, context length, capabilities |
| GET | `/api/v1/generation?id=` | Exact post-hoc token counts + cost for a completed request |
| GET | `/api/v1/models/{model}/endpoints` | Which providers serve a model (availability verification) |
| GET | `/api/v1/auth/key` | Stub — key/rate-limit info (so probes don't 404) |
| GET | `/api/v1/credits` | Stub — balance/usage (so probes don't 404) |

Auth: seren-router validates the single static bearer key the Gateway forwards (the same header position OpenRouter used, `Authorization`).

## Internal components

1. **Model catalog + normalization** — the source of truth for which `vendor/model` slugs exist, their context window, capabilities, price, and *which providers serve each slug*. Replaces the `openrouter.ai/api/v1/models` feed. Backs `GET /api/v1/models`.

2. **Provider registry** — one declarative entry per inference host (Together, Fireworks, Blackbox, DeepInfra, …): base URL, auth style, which Seren key to use, and the slug → provider-model-id mapping. See `docs/03`.

3. **Provider adapters** — per-host normalization shims. Most hosts are already OpenAI-compatible, so adapters are thin: translate request/response quirks and map each provider's native usage numbers into a unified `usage.cost`.

4. **Router core** — takes a model slug + routing preference, ranks candidate providers, picks the top healthy one, attaches Seren's key, streams the response, computes cost, and fails over to the next provider on error. See `docs/02`.

5. **Secrets store** — Seren's own per-provider API keys, referenced by name, rotated independently of the Gateway. See `docs/03`.

Everything OpenRouter did for us now lives in these five parts, under a contract the rest of Seren already speaks.
