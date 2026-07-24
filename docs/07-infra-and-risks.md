<!-- ABOUTME: Repo, infrastructure, and the honest risk register for seren-router.
ABOUTME: Covers the stack decision, deployment shape, and the risks of owning the router. -->

# 07 — Infrastructure & Risks

## Repo & stack

- **Repo:** `serenorg/seren-router` (private). This repo.
- **Stack — open decision.** Candidates:
  - **Rust** — matches Seren core; best for a money-path streaming proxy; high throughput; strong cost-accuracy guarantees. *Current lean*, because seren-router fronts all model traffic and cost correctness matters.
  - **TypeScript/Node** — fastest to parity; richest OpenAI-compatible ecosystem.
  - **Go** — proxy-shaped, good streaming.

  Decide deliberately before implementation starts. The lean is Rust for the hot path; Node is a legitimate "get to working router sooner" choice.

## Infrastructure shape

- A horizontally-scalable HTTP service with **first-class SSE streaming** and long-lived connection handling.
- **Secrets manager** for provider keys (referenced by name from the registry).
- **Datastore** for the model catalog + per-request cost ledger.
- **Scheduler** for the catalog sync job.
- **Observability**: per-provider latency / error-rate / cost dashboards, and alerting on the 30-second outage gate tripping.
- **Network posture**: private — only the Gateway calls it; it calls out to provider APIs. Deploy near the providers to protect latency.

## Risk register (honest)

1. **Single point of failure.** Post-migration, seren-router fronts *every* model call. It must be at least as reliable as OpenRouter — HA, health checks, a failover tier that does real work. The OpenRouter-as-fallback phase (`docs/05`) hides this early; after we remove that fallback, this is the critical path. **Mitigation:** multi-instance HA, keep a break-glass path to re-point `api_url` back to OpenRouter until the deal closes.

2. **Operational finance.** Paying N providers directly means N billing relationships, N prepay/credit balances, N invoices to reconcile against the ledger (`docs/04`). OpenRouter absorbed this. The margin gain is real but not free. **Mitigation:** start with a small set of high-volume providers; add the reconciliation ledger before scaling provider count.

3. **Slug/catalog curation** is the ongoing moat *and* the ongoing cost (`docs/03`). **Mitigation:** unmapped provider models are surfaced for review, never silently dropped.

4. **Data privacy & ToS.** Going direct means Seren must vet each provider's data-retention/training policy and confirm their ToS permits reselling inference under Seren's brand. **Verify per provider — do not assume.** Blackbox AI in particular could not be confirmed as an OpenRouter provider during design, so it is both a genuine additive win *and* an unvetted ToS/data question.

5. **Feature-parity scope.** OpenRouter also does prompt caching passthrough, structured outputs, moderation, vision/PDF handling, tool-call normalization quirks. Decide which reach parity in the MVP vs later. **Mitigation:** the OpenRouter fallback covers any feature not yet reimplemented, so parity can be incremental.

## The bet

Owning the router carries real reliability and operational-finance cost. The upside is removing a critical dependency that is being acquired by a payments competitor, and capturing OpenRouter's former margin. Given the strategic exposure, the risk is judged worth taking.
