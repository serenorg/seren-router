<!-- ABOUTME: Defines the internal-beta Blackbox GLM 5.2 route and its commercial boundary.
ABOUTME: Pins paid-meter pricing, credential scope, compatibility, and rollout evidence. -->

# 13 — Blackbox GLM 5.2 Internal Beta

## Decision and scope

Blackbox `z-ai/glm-5.2` is an enabled direct route only for the credential-bound
`beta` profile. It appears as an additional endpoint under the existing canonical
`z-ai/glm-5.2` model; it does not create a duplicate model listing. OpenRouter remains
the concrete fallback in beta and the only GLM 5.2 route in production.

The account belongs to `taariq@serendb.com`, displayed a `$20.00` balance on
2026-07-30, and had no active subscription. The route uses the key named
`seren-router-glm52-beta`, created with a `$1` monthly spend cap. Its value belongs
only in the deployment secret manager and AgentGateway environment; the registry and
rendered config contain only `$SEREN_ROUTER_KEY_BLACKBOX`.

## Commercial boundary

The standard Blackbox Terms prohibit reselling, commercially exploiting, or making
the service available to third parties. The account is not governed by a separately
reviewed enterprise agreement. Blackbox's Pro privacy page advertises zero data
retention and no training, but that does not grant customer-serving resale rights.

The route is therefore limited to Seren personnel using the private beta credential.
Production and customer-facing requests stay on OpenRouter. Any customer activation
requires retained written Blackbox permission or enterprise terms plus a separate
pricing decision. Disabling the row and removing the AgentGateway secret reference
is the immediate rollback.

## API and compatibility contract

Blackbox exposes an OpenAI-compatible API at `https://api.blackbox.ai/v1` with bearer
authentication. Authenticated `/models` discovery returned the exact provider model
ID `z-ai/glm-5.2`. The mapping accepts Chat Completions only. Both non-streaming JSON
and streaming SSE probes succeeded; the stream returned terminal token usage followed
by exactly one `[DONE]`.

Legacy Completions remain on OpenRouter. Provider-selection fields cannot force
Blackbox, and production credentials cannot opt into beta with request bodies or
headers. The generated AgentGateway production alias contains only OpenRouter; its
beta alias contains Blackbox priority 0 followed by OpenRouter priority 255.

## Exact economics

The public Blackbox GLM 5.2 page listed:

| Token class | Public page | Repeated paid meter |
| --- | ---: | ---: |
| Uncached prompt | $1.40 / MTok | $1.40 / MTok |
| Cached prompt | $0.26 / MTok | $0.14 / MTok |
| Completion | $4.40 / MTok | $4.40 / MTok |

Two paid JSON responses independently reconciled to the `$0.14/MTok` cache-read
meter. The repeated response reported 17 prompt tokens, including 16 cached, and four
completion tokens:

```text
(1 × $1.40 + 16 × $0.14 + 4 × $4.40) / 1,000,000
= $0.0000212400
```

The registry pins those observed gross rates for exact request accounting and records
the public `$0.26` discrepancy for reconciliation. If a response omits or reports an
invalid cached-token count, `provider_cost_usd` remains null rather than estimating.

The canonical customer sell row remains `$0.70/MTok` prompt and `$2.20/MTok`
completion. Blackbox is therefore loss-making on uncached and completion tokens before
the Gateway fee. Credits are reconciled separately and never reduce recorded gross
provider cost. This is an internal resilience route, not a margin route.

## Paid preflight evidence

The 2026-07-30 direct preflight made exactly three bounded requests with the
`$1`-capped key:

- one JSON completion returned 17 prompt tokens, nine cached tokens, four completion
  tokens, and `$0.0000300600` provider cost;
- one SSE completion returned 17 prompt tokens, 16 cached tokens, four completion
  tokens, terminal usage, and one `[DONE]`; and
- one repeated JSON completion returned the same 17/16/4 usage and
  `$0.0000212400` provider cost.

The first JSON request completed in 2.310 seconds and the SSE request in 1.461
seconds. The probes proved the API surface and exact cached meter without exhausting
the bounded credential. Deployment still requires the checked registry, pinned
AgentGateway validation, beta/production catalog proof, generation-ledger attribution,
a bounded routed canary, OpenRouter failover, and rollback/restore evidence.
