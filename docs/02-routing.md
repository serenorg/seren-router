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

For routing comparisons, one provider/model's price is its exact input-plus-output
per-million-token rate. This is the same coherent price pair used by the model catalog;
the request ledger still calculates actual cost from the observed prompt/completion
token mix.

Crucially, this is **not** a naive `provider.sort: throughput`. That mode disables load
balancing and routes sequentially to the single fastest provider — concentrating all
traffic onto one host, which then slows under load and hits rate limits. We keep
load-balancing and diversification; we just bias the weighting toward speed within a
price bound.

### Stability requirements for throughput weighting

Throughput differs from price in one way that matters for control: price is exogenous (it does not change based on how much traffic we send), but measured tokens/sec is **endogenous** — the more traffic we route to the fastest host, the slower it measures. That feedback is negative, so the system self-corrects rather than herding, but an undamped negative feedback loop oscillates: traffic flaps between hosts as measurements chase load. The default weighting therefore REQUIRES three stabilizers:

1. **Smoothed measurements.** Weight on a rolling-average throughput (e.g. EWMA over a multi-minute window), never on instantaneous readings.
2. **Hysteresis.** A provider must be meaningfully faster (configurable threshold) before weight shifts toward it, and shifts are rate-limited. No flapping on noise.
3. **Max-share cap.** No provider receives more than a configured share of a model's traffic (e.g. 60%), regardless of how fast it measures. Diversification is structurally guaranteed, not emergent.

The incumbent for hysteresis is the provider with the largest count in the rolling
per-model traffic window (a tie goes to the most recently selected tied provider). A
challenger inside the hysteresis threshold receives the incumbent's throughput weight,
so noise does not move traffic; a materially faster challenger keeps its higher weight.
The max-share limit is a discrete rolling-window quota:
`ceil(max_share × window_length)`. During initial window warm-up that rounding is
unavoidable; once the window is full, the configured share is exact whenever the
window size makes it representable.

Scale note, honestly: at Seren's current volume our traffic cannot meaningfully load any major host — these stabilizers are for the scale we are building toward. They are cheap to build in from day one, so they are in the MVP.

### Reference: OpenRouter's default (offered as the `balanced` mode)

OpenRouter's own default — offered in Seren as the explicit `balanced` mode — is:

1. Prioritize providers with no significant outage in the last 30 seconds.
2. Among stable providers, select from the lowest-cost candidates, **weighted by the inverse square of price**.
3. Remaining providers as fallbacks.

It optimizes for cheap-and-stable and is the right default for a general platform — just not for our latency-sensitive base.

## Explicit `provider.sort` overrides

When a `provider.sort` preference is supplied, load-balancing is **disabled** and
providers are tried **sequentially in ranked order**:

| `provider.sort` | Behavior |
| --- | --- |
| `price` | Lowest cost first |
| `throughput` | Highest tokens/sec first — the "fastest toks/sec" mode our customers ask for |
| `latency` | Fastest time-to-first-token first |

## Model-slug shortcuts

- `:nitro` — equivalent to `provider.sort: throughput` (maximize speed)
- `:floor` — equivalent to `provider.sort: price` (minimize cost)

Both disable load-balancing in favor of explicit optimization. They ride the existing model-id field, so any API-level or headless caller can use them with no other change.

## "Fastest toks/sec for the price" — now the Seren default

This customer-demanded behavior *is* Seren's default (see above): throughput-biased, price-capped load balancing. Users don't have to opt in.

For a caller who wants absolute maximum speed and is willing to give up
load-balancing/diversification, `:nitro` / `provider.sort: throughput` still applies the
stricter sequential form (top-throughput provider first, in order). The live
per-`(provider, model)` throughput and price measurements power both. Measurements and
rolling traffic-share state are partitioned by authenticated routing profile, so beta
experiments cannot steer production selection.

## Failover

Before response bytes are committed, a transport error, `429`, or `5xx` from the
selected concrete route causes one retry to the next-priority eligible concrete
route. Selecting the fallback from the already-filtered candidate set preserves
served-provider attribution and cannot reintroduce an incompatible provider. The
profile comes from the matched Gateway credential, never from a client header. The
sidecar also has a two-attempt retry policy on each generated LLM route so each
concrete request survives a transient upstream failure.
Once streaming bytes have been committed, neither a malformed event nor a later stream
error is replayed: doing so could duplicate billable output or concatenate two
providers into one response.

Before ranking, the router also filters provider/model routes against the requested
Chat or legacy completion endpoint, streaming support, and any declared `top_p`
interval. A non-null `top_p` must be a JSON number and a non-null `stream` must be a
JSON boolean, or the router returns `400` without contacting a provider. The fallback
is chosen only from the filtered routes under the same preference and price policy.
Generated profile aliases remain available for AgentGateway configuration validation
but are never used to bypass request compatibility.

## Health / circuit-breaking

Per-provider rolling error-rate and latency counters drive the 30-second outage gate: a degraded host is temporarily ejected from ranking, then probed back in. This is what keeps one provider's outage from becoming a Seren outage.

## Where the preference comes from

Two paths, both supported:

1. **Client toggle (Fastest / Balanced / Cheapest).** A small control near the model
   picker in Seren Desktop; the orchestrator threads the chosen mode through
   `UserCapabilities` into the `chat/completions` body under the OpenRouter-compatible
   `provider.sort` field. **Fastest is the default** (no field sent → throughput-biased,
   price-capped load balance); **Balanced** requests OpenRouter's price-weighted
   algorithm; **Cheapest** sends `provider.sort: price`. The external spelling for
   Balanced must be specified before that later client feature is implemented, because
   an omitted preference already means Seren's Fastest default. See `docs/06`.
2. **Slug suffix (`:nitro` / `:floor`).** Zero-UI power-user path.

## MVP vs later

- **MVP:** 30-second outage gate, the throughput-biased price-capped default weighting **with its three stabilizers (smoothing, hysteresis, max-share cap)**, the price-weighted `balanced` weighting, the three `provider.sort` modes, `:nitro`/`:floor` parsing, sequential failover.
- **Later:** weighted load-balancing refinements, data-policy routing, per-request `provider` allow/deny/order block (the seam that unlocks Shape C provider pinning — honored from day one but unused by the client at first).
