<!-- ABOUTME: The routing and failover policy for seren-router, mirroring OpenRouter exactly.
ABOUTME: Documents the default load-balancing, sort modes, slug shortcuts, and user-facing toggle. -->

# 02 — Routing & Failover Policy

seren-router replicates OpenRouter's routing behavior verbatim. This is the intelligence we are taking back in-house, so it is specified precisely.

## Seren's default (no preference specified): fastest-for-price

Seren diverges from OpenRouter here, on purpose. OpenRouter's default optimizes for a broad, cost-sensitive developer platform; Seren's customer base is latency-sensitive and repeatedly asks for the fastest tokens/sec they can get for the price. Owning the router is exactly what lets us set our own default.

**Seren default = throughput-biased, price-capped load balance:**

1. **Prioritize providers with no significant outage in the last 30 seconds** (same stability gate as OpenRouter).
2. Filter to providers **at or below a configured price ceiling** (so "fast" can't drag in an expensive host).
3. Among those, **load-balance weighted toward measured throughput** (fastest-for-price), rather than weighted toward inverse-square-price.
4. Remaining providers act as fallbacks.

Crucially, this is **not** a naive `sort: throughput`. That mode disables load balancing and routes sequentially to the single fastest provider — at our scale that concentrates all traffic onto one host, which then slows under load and hits rate limits. We keep load-balancing and diversification; we just bias the weighting toward speed within a price bound.

### Reference: OpenRouter's default (offered as the `balanced` mode)

OpenRouter's own default — offered in Seren as the explicit `balanced` mode — is:

1. Prioritize providers with no significant outage in the last 30 seconds.
2. Among stable providers, select from the lowest-cost candidates, **weighted by the inverse square of price**.
3. Remaining providers as fallbacks.

It optimizes for cheap-and-stable and is the right default for a general platform — just not for our latency-sensitive base.

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

## "Fastest toks/sec for the price" — now the Seren default

This customer-demanded behavior *is* Seren's default (see above): throughput-biased, price-capped load balancing. Users don't have to opt in.

For a caller who wants absolute maximum speed and is willing to give up load-balancing/diversification, `:nitro` / `sort: throughput` still applies the stricter sequential form (top-throughput provider first, in order). The live per-`(provider, model)` throughput and price measurements power both.

## Failover

On `429`, `5xx`, timeout, or a malformed stream, the router transparently retries down the ranked list (the fallback tier) before surfacing an error to the caller. "Automatic failover" is already advertised in the current `seren-models` description, so this is table stakes.

## Health / circuit-breaking

Per-provider rolling error-rate and latency counters drive the 30-second outage gate: a degraded host is temporarily ejected from ranking, then probed back in. This is what keeps one provider's outage from becoming a Seren outage.

## Where the preference comes from

Two paths, both supported:

1. **Client toggle (Fastest / Balanced / Cheapest).** A small control near the model picker in Seren Desktop; the orchestrator threads the chosen mode through `UserCapabilities` into the `chat/completions` body as a `sort` field. **Fastest is the default** (no field sent → throughput-biased, price-capped load balance); **Balanced** requests OpenRouter's price-weighted algorithm; **Cheapest** sends `sort: price`. See `docs/06`.
2. **Slug suffix (`:nitro` / `:floor`).** Zero-UI power-user path.

## MVP vs later

- **MVP:** 30-second outage gate, the throughput-biased price-capped default weighting, the price-weighted `balanced` weighting, the three `sort` modes, `:nitro`/`:floor` parsing, sequential failover.
- **Later:** weighted load-balancing refinements, data-policy routing, per-request `provider` allow/deny/order block (the seam that unlocks Shape C provider pinning — honored from day one but unused by the client at first).
