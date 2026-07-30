<!-- ABOUTME: Defines exact provider-cost reporting and Gateway-fee boundaries. -->
<!-- ABOUTME: Gives reconciliation formulas and mixed-version ledger semantics. -->

# 04 — Billing & Cost Accounting

## Money-path contract

`usage.cost` is the exact USD cost of the provider that served the response. Seren
Gateway reads that value through `upstream_cost_response_path: "usage.cost"` and
applies its configured fee once. The router does not maintain or apply a second sell
price, markup, or customer-billing policy.

The served provider, response usage, injected cost, and generation ledger row must
agree. A trustworthy numeric `usage.cost` returned by the served provider takes
precedence because it is the provider's metered amount. Otherwise the router computes
cost from the reviewed registry rates. Every calculation uses
`rust_decimal::Decimal`, divides per-million-token rates by exactly `1_000_000`, rounds
once to ten decimal places with midpoint-away-from-zero, and stores
`NUMERIC(18, 10)`.

## Exact formulas

For a model without a distinct cache-read price:

```text
provider_cost_usd =
  (
    prompt_tokens × input_price_per_mtok
    + completion_tokens × output_price_per_mtok
  ) / 1_000_000
```

For a mapping with `cached_input_price_per_mtok`:

```text
uncached_prompt_tokens = prompt_tokens - cached_prompt_tokens
provider_cost_usd =
  (
    uncached_prompt_tokens × input_price_per_mtok
    + cached_prompt_tokens × cached_input_price_per_mtok
    + completion_tokens × output_price_per_mtok
  ) / 1_000_000
```

When registry-rate fallback is required, the cached formula requires an integer
`usage.prompt_tokens_details.cached_tokens` no greater than
`usage.prompt_tokens`. If the mapping declares a cache-read price and the response
omits or corrupts that count, the router fails closed. It does not treat the missing
count as zero, charge all prompt tokens at one rate, or write an unresolved generation
row. A trustworthy upstream-reported cost does not require the router to reconstruct
cache usage.

## Registry and catalog

Each provider/model mapping in `registry/providers.yaml` contains that route's exact
input, optional cached-input, and output rates. There is no route-independent price
table.

`GET /api/v1/models/{model}/endpoints` exposes the exact per-token price for every
available provider endpoint. `GET /api/v1/models` has one canonical model row, so it
deterministically exposes the cheapest enabled endpoint by the existing
input-plus-output ordering. Route selection remains governed by the routing profile,
request preference, health, price ceiling, and measurements; the aggregate catalog
row is not a promise that every request uses that endpoint.

## JSON, SSE, and generation metadata

For non-streaming responses, the router injects provider cost at `usage.cost`. For
streaming responses, it injects the same amount into the terminal usage event before
exactly one `[DONE]`.

Both paths write:

- the credential-selected routing profile;
- canonical model slug;
- immutable internal provider ID;
- prompt and completion tokens;
- exact provider cost; and
- latency and response status.

`GET /api/v1/generation?id=` returns that same exact amount as `data.total_cost`.
Optional public aliases affect only public provider naming; internal attribution and
reconciliation retain the provider ID.

## Ledger compatibility

Migration `0003_generation_pricing_policy.sql` introduced
`provider_cost_usd` and `sell_price_usd` during the superseded two-price
implementation. Those columns remain in place to keep rollback and historical rows
readable:

- new binaries write the exact provider amount to `cost_usd` and
  `provider_cost_usd`;
- new binaries leave the historical `sell_price_usd` column null;
- historical rows with a non-null `sell_price_usd` continue returning the amount that
  was originally reported; and
- legacy rows with only `cost_usd` continue returning that value.

No migration rewrites historical request amounts.

## Reconciliation

New provider-cost rows can be aggregated with:

```sql
SELECT
    date_trunc('day', created_at) AS day,
    routing_profile,
    provider_id,
    canonical_slug,
    COUNT(*) AS requests,
    SUM(provider_cost_usd) AS provider_cost_usd
FROM generations
WHERE provider_cost_usd IS NOT NULL
  AND sell_price_usd IS NULL
GROUP BY 1, 2, 3, 4
ORDER BY 1, 2, 3, 4;
```

Compare those amounts with provider usage exports or invoices. Investigate missing
terminal usage, invalid cached-token telemetry, served-provider drift, ledger-write
failures, registry-price drift, or a Gateway charge that applies its fee more than
once.

Credits and grants may reduce the provider's invoice, but they do not change the
request's metered provider cost. Reconcile credit consumption separately. No prompt,
response body, customer identifier, authorization header, or credential is needed for
cost reconciliation.
