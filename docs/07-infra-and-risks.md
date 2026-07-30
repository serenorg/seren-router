<!-- ABOUTME: Repo, infrastructure, and the honest risk register for seren-router.
ABOUTME: Covers the stack decision, deployment shape, and the risks of owning the router. -->

# 07 — Infrastructure & Risks

## Repo & stack

- **Repo:** `serenorg/seren-router` (private). This repo.
- **Stack — DECIDED: Rust.** Rationale:
  - **Team expertise.** The team already knows Rust, which neutralizes Node's main advantage (time-to-parity) — our ramp cost is low.
  - **Integrates with the rest of the stack.** The Seren core, Gateway, and orchestrator are Rust; shared crates, types, and tooling carry over.
  - **Fewer bugs — of the categories that matter here.** For a high-concurrency, money-path streaming proxy, Rust's ownership model and type system eliminate whole classes of defect (data races, use-after-free, null derefs). It does *not* prevent logic / billing / routing bugs — those still need tests — but it removes exactly the concurrency and memory failures that are hardest to reproduce under streaming load.

  Accepted tradeoff: slower to a first working version than Node, offset by team familiarity.

- **Foundation — DECIDED: agentgateway** (2026-07-24, see `docs/09`). We do not hand-write the streaming proxy, provider adapters, failover, or load balancer; we build a thin OpenRouter-compat + pricing + cost layer on agentgateway's Apache-2.0 Rust core. Verified by source inspection and a live functional test (real local model; failover proven with `health.eviction`). New risk to track: upstream coupling to an alpha-channel project — pin a vetted revision; LF governance + Apache-2.0 make forking a real escape hatch.

  Rejected: **TypeScript/Node** (fastest to parity and richest OpenAI-compat ecosystem, but a separate runtime from our core with weaker concurrency guarantees) and **Go** (proxy-shaped, but no ecosystem/integration advantage over Rust for us).

## Infrastructure shape

- A horizontally-scalable HTTP service with **first-class SSE streaming** and long-lived connection handling.
- **Secrets manager** for provider keys (referenced by name from the registry).
- **Datastore** for the model catalog + per-request cost ledger.
- **Scheduler** for the catalog sync job.
- **Observability**: per-provider latency / error-rate / cost dashboards, and alerting on the 30-second outage gate tripping.
- **Network posture**: private — only the Gateway calls it; it calls out to provider APIs. Deploy near the providers to protect latency.

## Risk register (honest)

1. **Single point of failure.** Post-migration, seren-router fronts *every* model call. It must be at least as reliable as OpenRouter — HA, health checks, a failover tier that does real work. The OpenRouter-as-fallback phase (`docs/05`) hides this early; after we remove that fallback, this is the critical path. **Mitigation:** multi-instance HA, keep a break-glass path to re-point `api_url` back to OpenRouter until the deal closes.

2. **Operational finance.** Paying N providers directly means N balances and invoices
   to reconcile against the ledger (`docs/04`). **Mitigation:** start with a small set
   of high-volume providers and reconcile exact serving-provider cost before scaling.

3. **Slug/catalog curation** is the ongoing moat *and* the ongoing cost (`docs/03`). **Mitigation:** unmapped provider models are surfaced for review, never silently dropped.

4. **Feature-parity scope.** OpenRouter also does prompt caching passthrough,
   structured outputs, moderation, vision/PDF handling, and tool-call normalization.
   Decide which reach parity in the MVP vs later. **Mitigation:** the OpenRouter
   fallback covers any feature not yet implemented, so parity can be incremental.

## The bet

Owning the router carries real reliability and operational-finance cost. The upside is
removing a critical dependency that is being acquired by a payments competitor and
controlling provider selection directly. Given the strategic exposure, the risk is
judged worth taking.
