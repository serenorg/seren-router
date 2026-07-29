<!-- ABOUTME: Defines the reviewed sell-price, provider-cost, and Gateway-fee contract.
ABOUTME: Gives exact reconciliation formulas and rollout-safe ledger semantics. -->

# 04 — Billing & Cost Accounting

## Approved policy

Decision date: 2026-07-27. The owner approved a **reviewed sell-price layer** for
direct-provider routing.

The customer-facing pre-Gateway price for a canonical model stays at its reviewed
`sell_prices` registry rate regardless of which provider serves the request. The
initial sell prices equal the incumbent OpenRouter rates. A cheaper direct route
therefore leaves the customer's documented price unchanged and converts the removed
middleman spread into Seren router gross margin.

Provider cost remains exact and separate. It is never substituted for the sell price
or estimated. When the upstream response lacks details required for exact provider
cost, the sell subtotal is still recorded and `provider_cost_usd` is persisted as
null for reconciliation.

## The three amounts

For one successful generation:

1. **Provider cost** is the served provider's reviewed input/output cost multiplied by
   actual token usage. When a provider publishes a distinct cached-input rate,
   reported cached prompt tokens use that rate and the remaining prompt tokens use
   the ordinary input rate.
2. **Sell subtotal** is the canonical model's reviewed input/output sell price
   multiplied by the same token usage.
3. **Gateway fee** is the existing 5% fee applied by Seren Gateway to the sell
   subtotal.

All router arithmetic uses `rust_decimal`, divides per-million-token rates by exactly
`1_000_000`, rounds once to ten decimal places with midpoint-away-from-zero, and
stores `NUMERIC(18, 10)`. No binary floating-point value enters billing.

For a provider mapping with a cached-input price:

```text
uncached_prompt_tokens = prompt_tokens - cached_prompt_tokens
provider_cost_usd =
  (
    uncached_prompt_tokens × input_price_per_mtok
    + cached_prompt_tokens × cached_input_price_per_mtok
    + completion_tokens × output_price_per_mtok
  ) / 1_000_000
```

The upstream response must supply
`usage.prompt_tokens_details.cached_tokens`, and that count must not exceed
`usage.prompt_tokens`. Customer sell pricing continues to apply the canonical
input rate to all prompt tokens. If a provider mapping declares a cached-input
rate but the exact count is missing or invalid, the router still injects the
exact sell subtotal and records the generation with a null provider cost. It
never guesses the provider cost; reconciliation and provider activation gates
must treat that null as unresolved.

The router reports only the sell subtotal at `usage.cost`. Gateway continues to read
`upstream_cost_response_path: "usage.cost"` and add its 5% exactly once. The router
does not add, estimate, or embed the Gateway fee.

## Exact Llama example

The reviewed 72-hour token mix was 3,962 prompt tokens and 214 completion tokens:

| Route | Sell subtotal | Provider cost | Router gross margin | Gateway fee | Customer total |
| --- | ---: | ---: | ---: | ---: | ---: |
| OpenRouter fallback | $0.0006006600 | $0.0006006600 | $0.0000000000 | $0.0000300330 | $0.0006306930 |
| DeepInfra direct | $0.0006006600 | $0.0004646800 | $0.0001359800 | $0.0000300330 | $0.0006306930 |

The intended router gross-margin metric is:

```text
router_gross_margin_usd = sell_price_usd - provider_cost_usd
router_gross_margin_percent = router_gross_margin_usd / sell_price_usd × 100
```

For the direct row above, router gross margin is 22.6384310592% of the sell subtotal.
The Gateway fee is reported separately; combining it with router margin would obscure
which layer earned the revenue.

If a reliability fallback costs more than the reviewed sell price, the customer price
does not rise. Reconciliation reports negative router margin and operations must
review or disable that route.

## Registry and catalog contract

`registry/providers.yaml` has two independent price sources:

- top-level `sell_prices` contains one reviewed pre-Gateway customer price per
  canonical slug;
- each provider model mapping contains that provider's input/output cost.

Every provider mapping, including a disabled canary, must reference exactly one
non-negative sell-price row. Missing, duplicate, or negative sell prices fail registry
validation. Missing, duplicate, or negative enabled-provider costs fail price-table
construction.

`GET /api/v1/models` and every model endpoint expose `sell_prices` as per-token
`pricing.prompt` and `pricing.completion`. Provider cost is intentionally not exposed
as customer pricing. Consequently, all endpoints for one canonical slug publish the
same price even when their underlying costs differ.

A sell-price change requires owner review, an exact-decimal fixture update, and a
coordinated catalog/Gateway review. Enabling or changing a provider cost cannot
silently change the customer price.

## JSON, streaming, and generation lookup

For non-streaming responses, the router injects the computed sell subtotal into
`usage.cost`. For streaming responses it injects the same value into the terminal
usage event before `[DONE]`.

Both paths persist the exact sell subtotal under the provider response ID. They also
persist exact provider cost when it can be calculated; otherwise
`provider_cost_usd` remains null and is never estimated. `GET
/api/v1/generation?id=` returns the sell subtotal as `data.total_cost`, matching the
metered response. It does not expose the internal provider cost. When a provider has
public catalog aliases, generation metadata uses its public display name while the
ledger retains the immutable internal provider ID for reconciliation.

## Ledger and mixed-version safety

New rows contain:

- `provider_cost_usd`: exact cost expected from the served provider, or null when
  required upstream usage detail is unresolved;
- `sell_price_usd`: exact pre-Gateway customer subtotal;
- `cost_usd`: a rollback-compatible mirror of `sell_price_usd`.

Migration `0003_generation_pricing_policy.sql` backfills both new columns from legacy
`cost_usd`. Before this policy, `cost_usd` was provider cost, so historical backfilled
rows correctly have zero inferred router margin. New binaries write all three
columns. An older binary can still write `cost_usd` during rollback without a schema
failure; such a row has null new columns and must be treated as legacy.

## Reconciliation

Aggregate only rows with both new amounts present:

```sql
SELECT
    date_trunc('day', created_at) AS day,
    routing_profile,
    provider_id,
    canonical_slug,
    COUNT(*) AS requests,
    SUM(provider_cost_usd) AS provider_cost_usd,
    SUM(sell_price_usd) AS sell_price_usd,
    SUM(sell_price_usd - provider_cost_usd) AS router_gross_margin_usd
FROM generations
WHERE provider_cost_usd IS NOT NULL
  AND sell_price_usd IS NOT NULL
GROUP BY 1, 2, 3, 4
ORDER BY 1, 2, 3, 4;
```

Operations reconcile `provider_cost_usd` against provider usage exports and invoices,
then reconcile `sell_price_usd` against Gateway upstream-cost metering. Investigate:

- missing amounts or ledger-write failures;
- provider invoice drift beyond the provider-specific tolerance;
- `cost_usd <> sell_price_usd` on new rows;
- negative router gross margin;
- catalog sell prices that differ from the checked-in registry; and
- any Gateway charge that applies the 5% fee more than once.

Promotional credits do not reduce request-level provider cost. The ledger records
gross published or contracted provider cost; billing reconciliation records credit
consumption separately so temporary grants do not masquerade as durable margin.

No prompt, response body, customer identifier, authorization header, or credential is
needed for this reconciliation.
