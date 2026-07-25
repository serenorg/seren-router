<!-- ABOUTME: The seren-desktop client-side changes required for the seren-router migration.
ABOUTME: Cutting the last direct openrouter.ai dependency plus the one new routing toggle. -->

# 06 — seren-desktop Client Changes

Even in an invisible swap, the desktop client has one real dependency on OpenRouter that is **not** the Gateway: it fetches its model catalog directly from `openrouter.ai`. That must go. Four changes total, only one of which is a feature.

**Sequencing (resolved by owner decision, 2026-07-24).** Changes 1–3 (catalog repoint, allowlist removal, naming cleanup) SHIPPED ahead of the cutover: seren-desktop PR #3292, reverted in #3294 during sequencing review, reinstated and merged in #3297 after Taariq's review. The deciding argument: desktop clients update on a release + auto-update lag, so landing the repoint early means the installed fleet is already pointed at the Gateway when the server-side cutover happens — cutover then requires zero client release. Accepted tradeoffs, recorded for operations: the catalog now traverses the Gateway (egress + availability coupling), and signed-out model search uses the hardcoded fallback list instead of the public catalog. Change 4 (the routing toggle) remains Phase 5 work.

## 1. Repoint the catalog fetch

- **Where:** `src/services/models.ts:23` — `OPENROUTER_MODELS_URL = "https://openrouter.ai/api/v1/models"`.
- **Change:** fetch through the Gateway publisher's own catalog endpoint instead — `GET /publishers/seren-models/models` — which already exists and now proxies to seren-router's `/api/v1/models`.
- **Why:** removes the direct third-party dependency and keeps auth consistent (goes through the Gateway with the user's bearer token). Only the *searchable* catalog hits OpenRouter today; the default picker list already comes from Seren.

## 2. Drop the allowlist entry

- **Where:** `src-tauri/tauri.conf.json:32` (CSP `connect-src`) and `src-tauri/capabilities/default.json:32` (URL permission).
- **Change:** remove `https://openrouter.ai`.
- **Why:** after change #1, the client never needs to reach `openrouter.ai` again. Closing the surface is the point.

## 3. Re-own the slug format (cosmetic)

- **Where:** `src/lib/providers/seren.ts:66` — `normalizeModelId()` normalizes to "OpenRouter format".
- **Change:** the `vendor/model` convention is deliberately kept as seren-router's canonical slug space, so the code stays; only the comment/ownership language changes.
- The Rust `reasoning: { effort }` field (`src-tauri/src/orchestrator/chat_model_worker.rs:612`) also stays — seren-router honors it — so there is no change there, only a router requirement.

## 4. The one feature: Fastest / Balanced / Cheapest toggle

- **New control** near the model picker (e.g. in `ModelSelector.tsx` or the composer).
- **Thread the chosen routing preference** through:
  - `UserCapabilities` (`src-tauri/src/orchestrator/types.rs`)
  - the `chat/completions` request body in `src-tauri/src/orchestrator/chat_model_worker.rs`
  - → the nested `provider.sort` field, which seren-router honors for
    `price | throughput | latency`.
- Specify the server-side Balanced wire value before implementing the toggle. Omitting
  `provider.sort` already selects Seren's Fastest default, while OpenRouter uses the
  omission for its own balanced algorithm.
- The `:nitro` / `:floor` slug suffixes work with **zero client change** as the power-user path.

## Footprint summary

No provider-registry churn, no new picker rows, no billing changes. Three small removals/repoints plus one opt-in toggle. This is the entire desktop cost of Shape A.
