<!-- ABOUTME: The cost-accounting contract for seren-router and how it fits Seren's existing billing.
ABOUTME: Distinguishes the customer-facing billing (untouched) from provider-facing cost accounting (rebuilt). -->

# 04 — Billing & Cost Accounting

"Copy OpenRouter's billing" needs precision, because there are **two** billing layers and we should rebuild only one. Rebuilding both would duplicate SerenBucks and fight the Gateway.

## Layer that stays untouched — customer-facing billing

These all live on the **Gateway** and keep working unchanged:

- SerenBucks debit
- the `gateway_fee_percent: 5%` markup
- the `ApiResultResponse` envelope (`cost` / `asset_symbol` / `payment_source` / `cost_breakdown`)
- prepaid and on-chain (x402) payment
- 402-on-insufficient-balance

We do **not** rebuild OpenRouter's customer credit system. Seren already has one.

## Layer we rebuild — provider-facing cost accounting

This is the OpenRouter billing behavior seren-router must copy.

### The critical contract: `usage.cost`

seren-router's single most important billing job is to **populate `usage.cost` (USD) accurately in every response body**, because the Gateway reads exactly that path (`upstream_cost_response_path: "usage.cost"`) to meter and mark up. For each call:

- compute the true provider cost — the chosen provider's token pricing × actual tokens, or the provider's own returned cost;
- report it at `usage.cost`.

### Streaming

For streamed responses, final cost is not known until the end. seren-router must:

- emit a **final usage chunk** carrying cost (as OpenRouter does), and
- back it with `GET /api/v1/generation?id=` for exact post-hoc lookup.

## Where the margin actually grows

- **Today:** Seren pays OpenRouter (provider rate **plus OpenRouter's cut**), then adds 5%.
- **After:** Seren pays the provider **directly** — no middleman cut — and still applies its markup.

seren-router reports the **true provider cost** and lets the Gateway own the markup, so pricing policy stays in one place. Margin expands by exactly OpenRouter's former take. That expansion is the economic case for this project.

## New back-office need: reconciliation

Because Seren now pays N providers directly, it needs a **per-request cost ledger** to reconcile metered `usage.cost` against the actual invoices each provider bills. This is operational finance work OpenRouter previously absorbed (see risks in `docs/07`).
