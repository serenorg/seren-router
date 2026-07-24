<!-- ABOUTME: The routing and failover policy for seren-router, mirroring OpenRouter exactly.
ABOUTME: Documents the default load-balancing, sort modes, slug shortcuts, and user-facing toggle. -->

# 02 — Routing & Failover Policy

seren-router replicates OpenRouter's routing behavior verbatim. This is the intelligence we are taking back in-house, so it is specified precisely.

## Default behavior (no preference specified)

Load-balance across providers, prioritizing price:

1. **Prioritize providers with no significant outage in the last 30 seconds.**
2. Among those stable providers, select from the lowest-cost candidates, **weighted by the inverse square of price** — cheaper providers get disproportionately more traffic, but not *always* the single cheapest.
3. Remaining providers act as fallbacks.

This is OpenRouter's proven default and stays our default. It already optimizes for cheap-and-stable, which is exactly the margin posture we want.

## Explicit `sort` overrides

When a `sort` preference is supplied, load-balancing is **disabled** and providers are tried **sequentially in ranked order**:

| `sort` | Behavior |
| --- | --- |
| `price` | Lowest cost first |
| `throughput` | Highest tokens/sec first — the "fastest toks/sec" mode our customers ask for |
| `latency` | Fastest time-to-first-token first |

## Model-slug shortcuts

- `:nitro` — equivalent to `sort: throughput` (maximize speed)
- `:floor` — equivalent to `sort: price` (minimize cost)

Both disable load-balancing in favor of explicit optimization. They ride the existing model-id field, so any API-level or headless caller can use them with no other change.

## "Fastest toks/sec for the price"

This is a first-class, customer-demanded mode. It maps to `sort: throughput` but with a **price ceiling filter**: rank candidate providers by measured throughput, but only among those at or below a configured price bound, so speed can't drag in an expensive host. The live per-`(provider, model)` throughput and price measurements the default ranking already needs are what power this.

## Failover

On `429`, `5xx`, timeout, or a malformed stream, the router transparently retries down the ranked list (the fallback tier) before surfacing an error to the caller. "Automatic failover" is already advertised in the current `seren-models` description, so this is table stakes.

## Health / circuit-breaking

Per-provider rolling error-rate and latency counters drive the 30-second outage gate: a degraded host is temporarily ejected from ranking, then probed back in. This is what keeps one provider's outage from becoming a Seren outage.

## Where the preference comes from

Two paths, both supported:

1. **Client toggle (Fastest / Balanced / Cheapest).** A small control near the model picker in Seren Desktop; the orchestrator threads the chosen mode through `UserCapabilities` into the `chat/completions` body as a `sort` field. See `docs/06`.
2. **Slug suffix (`:nitro` / `:floor`).** Zero-UI power-user path.

## MVP vs later

- **MVP:** 30-second outage gate, inverse-square price weighting, the three `sort` modes, `:nitro`/`:floor` parsing, sequential failover.
- **Later:** weighted load-balancing refinements, data-policy routing, per-request `provider` allow/deny/order block (the seam that unlocks Shape C provider pinning — honored from day one but unused by the client at first).
