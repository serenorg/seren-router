<!-- ABOUTME: Phased implementation plan for seren-router.
ABOUTME: Sequences the build from skeleton through full OpenRouter removal. -->

# 08 — Implementation Plan

Phased so each step is independently shippable and reversible. The guiding principle: **register OpenRouter as a fallback provider first**, so the service is a superset of today from day one and the migration has no coverage gaps.

## Phase 0 — Decisions & scaffolding

- [x] Lock the stack decision — **Rust** (`docs/07`, 2026-07-24).
- [x] Foundation evaluation — **build on agentgateway** (`docs/09`, 2026-07-24: source-verified + live functional test).
- [ ] Pin an agentgateway revision (vet the alpha channel; pick release/commit).
- [ ] Create infra skeleton: service, secrets manager, datastore, scheduler, observability.
- [ ] Define the canonical slug schema and the provider-registry schema (`docs/03`) + the registry→agentgateway-config compiler design.
- [ ] Confirm cost-parity measurement method against OpenRouter.

## Phase 1 — Compatibility skeleton (OpenRouter passthrough on agentgateway)

- [ ] Stand up agentgateway with **OpenRouter as the sole provider** (fallback role) via generated config.
- [ ] Build the seren-router layer: OpenRouter-compatible surface (`docs/01`) — `usage.cost` injection, `/generation`, aggregated `/models` with pricing, `provider.sort` / `:nitro` / `:floor`, `reasoning.effort` passthrough, stub `/auth/key` + `/credits`.
- [ ] Attach the retry route policy for **same-request failover** and `health.eviction` on every target (docs/09 sharp edges 1–2); functionally test both against a real dead upstream.
- [ ] Validate the static bearer key the Gateway forwards.
- [ ] Verify byte-compatible responses incl. `usage.cost` and the final streaming usage chunk (`docs/04`).
- [ ] **Gate:** a request through seren-router → OpenRouter is indistinguishable from today.

## Phase 2 — Canary the passthrough

- [ ] Stand up a beta publisher slug (or percentage split) pointing at seren-router.
- [ ] Run real traffic through the passthrough; compare latency, error rate, and `usage.cost` parity vs direct OpenRouter.
- [ ] **Gate:** parity confirmed on the models users actually call (checked against recent `seren-models` usage).

## Phase 3 — Cutover (still OpenRouter underneath)

- [ ] Repoint `seren-models.api_url` → seren-router; swap the key (`docs/05`).
- [ ] Monitor. **Instant revert = restore `api_url`.**
- [ ] **Gate:** production stable on seren-router with OpenRouter as the only (fallback) provider.

## Phase 4 — Direct providers (peel off)

- [ ] Add the first direct provider (Together): registry row + key + adapter + slug mappings.
- [ ] Confirm cheaper `usage.cost` and equal/better reliability; shift its slugs off the fallback.
- [ ] Repeat: Fireworks, Blackbox, DeepInfra, Novita, Baseten, … (prioritize by traffic volume × margin gain).
- [ ] Implement the routing policy in full (`docs/02`): outage gate, inverse-square price weighting, `sort` modes, `:nitro`/`:floor`, failover.
- [ ] Stand up the per-request cost ledger + provider-invoice reconciliation (`docs/04`).

## Phase 5 — seren-desktop client changes

- [ ] Repoint catalog fetch to `GET /publishers/seren-models/models` (`docs/06`).
- [ ] Remove `openrouter.ai` from the allowlist/CSP.
- [ ] Re-own slug-format comments.
- [ ] Ship the Fastest / Balanced / Cheapest toggle + thread `sort` through the orchestrator.
- [ ] **Sequencing: Phase 5 lands only AFTER the Phase 3 cutover is stable.** Repointing the client catalog before seren-router exists does not remove the OpenRouter dependency (the publisher endpoint proxies OpenRouter today) — it adds a Gateway hop and auth requirement to a previously public path. A premature attempt (seren-desktop PR #3292) was reverted (#3294) for exactly this; tracked in seren-desktop #3291.
- [ ] Preserve signed-out catalog behavior at implementation: the old openrouter.ai fetch was unauthenticated; the publisher path is authed, so signed-out users would silently drop to the hardcoded fallback list. Decide explicitly (public router `/models`, cached catalog, or accept the fallback) before shipping.
- [ ] Client repoint ships with its own canary/staged rollout per the docs/05 discipline — it is a customer-facing change of the same migration family.

## Phase 6 — Remove OpenRouter

- [ ] When direct coverage reaches the models users actually call, delete the OpenRouter fallback registry entry.
- [ ] Decommission the Seren-owned OpenRouter key.
- [ ] **Gate:** zero traffic resolves through OpenRouter. Dependency removed.

## Later (tracked separately)

- [ ] Absorb `google-gemini-3` (and any other OpenRouter-backed publishers) into seren-router.
- [ ] Provider pinning (Shape C) via the per-request `provider` block — seam already built in Phase 4.
- [ ] Feature parity extras: prompt caching, structured outputs, moderation.
