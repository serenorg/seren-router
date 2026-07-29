<!-- ABOUTME: Phased implementation plan for seren-router.
ABOUTME: Sequences the build from skeleton through full OpenRouter removal. -->

# 08 — Implementation Plan

Phased so each step is independently shippable and reversible. The guiding principle: **register OpenRouter as a fallback provider first**, so the service is a superset of today from day one and the migration has no coverage gaps.

## Phase 0 — Decisions & scaffolding

- [x] Lock the stack decision — **Rust** (`docs/07`, 2026-07-24).
- [x] Foundation evaluation — **build on agentgateway** (`docs/09`, 2026-07-24: source-verified + live functional test).
- [x] Pin an agentgateway revision (v1.4.0-alpha.2 / `6ab7285`, with reviewed platform digests).
- [ ] Create infra skeleton: service, secrets manager, datastore, scheduler, observability.
- [x] Define the canonical slug schema and the provider-registry schema (`docs/03`) + the registry→agentgateway-config compiler design.
- [x] Confirm cost-parity measurement method against OpenRouter (live M6 parity and
  per-generation timing normalization, 2026-07-25).

## Phase 1 — Compatibility skeleton (OpenRouter passthrough on agentgateway)

- [x] Stand up agentgateway with **OpenRouter as the sole provider** (fallback role) via generated config.
- [x] Build the seren-router layer: OpenRouter-compatible surface (`docs/01`) — `usage.cost` injection, `/generation`, aggregated `/models` with pricing, `provider.sort` / `:nitro` / `:floor`, `reasoning.effort` passthrough, stub `/auth/key` + `/credits`.
- [x] Attach the retry route policy for **same-request failover** and `health.eviction` on every target (docs/09 sharp edges 1–2); functionally test both against a real dead upstream.
- [x] Validate the static bearer key the Gateway forwards.
- [x] Verify compatible responses incl. `usage.cost`, preserved OpenRouter generation
  metadata, and both supported terminal streaming usage shapes (`docs/04`).
- [x] **Gate:** a request through seren-router → OpenRouter is indistinguishable from
  today (live JSON/SSE parity plus 100-request-per-path soak, 2026-07-25).

## Phase 2 — Canary the passthrough

- [ ] Stand up a beta publisher slug (or percentage split) pointing at seren-router.
- [ ] Run real traffic through the passthrough; compare latency, error rate, and `usage.cost` parity vs direct OpenRouter.
- [ ] **Gate:** parity confirmed on the models users actually call (checked against recent `seren-models` usage).

## Phase 3 — Cutover (still OpenRouter underneath)

- [ ] Repoint `seren-models.api_url` → seren-router; swap the key (`docs/05`).
- [ ] Monitor. **Instant revert = restore `api_url`.**
- [ ] **Gate:** production stable on seren-router with OpenRouter as the only (fallback) provider.

## Phase 4 — Direct providers (peel off)

- [x] Select the first direct provider engineering canary: DeepInfra Llama 3.3
  70B Turbo (`docs/11`, 2026-07-27). The registry entry is disabled and
  beta-only; legal, credential, and spend gates remain.
- [x] Add the disabled Modal Kimi K3 Shared API candidate with neutral public
  attribution, exact cached-input accounting, non-streaming-only beta constraints,
  and a credential-bound live gate (`docs/12`, 2026-07-29). It is not deployed.
- [x] Approve and implement route-independent sell pricing with separately
  reconciled provider cost (`docs/04`, 2026-07-27).
- [x] Pass the revised JSON-only Modal live gate with exact cached-token accounting
  and neutral public attribution (`docs/12`, 2026-07-29).
- [ ] Obtain written customer-serving permission and reconcile the reported
  promotional-credit grant before any customer-serving deployment. The checked
  Modal candidate remains disabled.
- [ ] Activate DeepInfra after the remaining `docs/11` gates: written provider
  consent, scoped key, live compatibility test, and spend cap.
- [ ] Confirm lower provider cost, invariant sell-price `usage.cost`, and
  equal/better reliability; shift its slugs off the fallback.
- [ ] Repeat: Fireworks, Blackbox, Novita, Baseten, … (prioritize by traffic volume × margin gain).
- [ ] Implement the routing policy in full (`docs/02`): outage gate, inverse-square price weighting, `sort` modes, `:nitro`/`:floor`, failover.
- [x] Stand up the per-request provider-cost/sell-price ledger and reconciliation
  contract (`docs/04`, 2026-07-27).

## Phase 5 — seren-desktop client changes

- [ ] Repoint catalog fetch to `GET /publishers/seren-models/models` (`docs/06`).
- [ ] Remove `openrouter.ai` from the allowlist/CSP.
- [ ] Re-own slug-format comments.
- [ ] Ship the Fastest / Balanced / Cheapest toggle + thread `sort` through the orchestrator.
- [x] Catalog repoint + allowlist removal SHIPPED early by owner decision (2026-07-24): #3292 → reverted #3294 → reinstated #3297. Rationale: fleet auto-update lag — clients must already point at the Gateway before the server-side cutover, so cutover needs no client release. Tradeoffs accepted in review: catalog traverses the Gateway; signed-out search falls back to the hardcoded list (closes seren-desktop #3291).
- [ ] Remaining Phase 5 scope: the Fastest/Balanced/Cheapest toggle + `sort` threading through the orchestrator (pairs with seren-router Phase 1 `provider.sort` support).

## Phase 6 — Remove OpenRouter

- [ ] When direct coverage reaches the models users actually call, delete the OpenRouter fallback registry entry.
- [ ] Decommission the Seren-owned OpenRouter key.
- [ ] **Gate:** zero traffic resolves through OpenRouter. Dependency removed.

## Later (tracked separately)

- [ ] Absorb `google-gemini-3` (and any other OpenRouter-backed publishers) into seren-router.
- [ ] Provider pinning (Shape C) via the per-request `provider` block — seam already built in Phase 4.
- [ ] Feature parity extras: prompt caching, structured outputs, moderation.
