<!-- ABOUTME: Phased implementation plan for seren-router.
ABOUTME: Sequences the build from skeleton through full OpenRouter removal. -->

# 08 — Implementation Plan

Phased so each step is independently shippable and reversible. The guiding principle: **register OpenRouter as a fallback provider first**, so the service is a superset of today from day one and the migration has no coverage gaps.

## Phase 0 — Decisions & scaffolding

- [ ] Lock the stack decision (Rust vs Node vs Go) — see `docs/07`.
- [ ] Create infra skeleton: service, secrets manager, datastore, scheduler, observability.
- [ ] Define the canonical slug schema and the provider-registry schema (`docs/03`).
- [ ] Confirm cost-parity measurement method against OpenRouter.

## Phase 1 — Compatibility skeleton (OpenRouter passthrough)

- [ ] Implement the OpenRouter-compatible API surface (`docs/01`): `/chat/completions` (streaming + tools + vision), `/models`, `/completions`, `/generation`, `/models/{model}/endpoints`, stub `/auth/key` + `/credits`.
- [ ] Register **OpenRouter as the sole provider** in the registry (fallback role).
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
- [ ] (These can land in parallel with Phase 4; they do not block cutover.)

## Phase 6 — Remove OpenRouter

- [ ] When direct coverage reaches the models users actually call, delete the OpenRouter fallback registry entry.
- [ ] Decommission the Seren-owned OpenRouter key.
- [ ] **Gate:** zero traffic resolves through OpenRouter. Dependency removed.

## Later (tracked separately)

- [ ] Absorb `google-gemini-3` (and any other OpenRouter-backed publishers) into seren-router.
- [ ] Provider pinning (Shape C) via the per-request `provider` block — seam already built in Phase 4.
- [ ] Feature parity extras: prompt caching, structured outputs, moderation.
