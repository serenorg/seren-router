<!-- ABOUTME: Technical activation record for Blackbox GLM 5.2. -->
<!-- ABOUTME: Records exact metering, compatibility, failover, and rollback evidence. -->

# 13 — Blackbox GLM 5.2

Activation record: 2026-07-30.

## Route

Blackbox model `z-ai/glm-5.2` serves the canonical `z-ai/glm-5.2` slug in
production and beta. It is the first direct fallback behind DeepInfra GLM; OpenRouter
remains the priority-255 final fallback in both profiles. The route accepts Chat
Completions, JSON and SSE.

The provider credential is stored only in the deployment secret boundary and mounted
only into AgentGateway. The provider ID remains internal; public catalog, response,
and generation metadata use the configured neutral alias.

## Exact metering

Repeated authenticated responses established these rates:

| Token class | Cost / MTok |
| --- | ---: |
| Uncached prompt | $1.40 |
| Cached prompt | $0.14 |
| Completion | $4.40 |

The public provider page displayed `$0.26/MTok` cache reads during the first review,
but repeated paid responses reconciled exactly at `$0.14/MTok`. The checked mapping
uses the authenticated meter. Issue #78 compares current published and measured GLM
5.2 provider economics again.

When Blackbox does not return its own numeric `usage.cost`, the route's cache-read
price requires `usage.prompt_tokens_details.cached_tokens`. Missing, nonnumeric, or
impossible cached-token telemetry fails closed rather than inventing a registry-rate
cost.

## Compatibility evidence

Direct preflight:

- `/v1/models` returned exact ID `z-ai/glm-5.2`;
- JSON HTTP 200: 17 prompt / 9 cached / 4 completion, `$0.0000300600`, 2.310 s;
- SSE HTTP 200: 17 prompt / 16 cached / 4 completion, terminal usage and one
  `[DONE]`, 1.461 s; and
- repeated JSON HTTP 200: 17 prompt / 16 cached / 4 completion,
  `$0.0000212400`, reconfirming the cache meter.

Pinned Linux ARM64 AgentGateway + router + PostgreSQL:

- JSON and SSE HTTP 200; each 18 prompt / 17 cached / 8 completion;
- exact provider cost `$0.0000389800` persisted for both generations;
- SSE terminal usage was costed and followed by one `[DONE]`; and
- nested `provider_specific_fields` was absent from every public response.

## Initial beta canary

The first deployed canary completed 10 requests: 5 JSON and 5 SSE, zero errors,
197 prompt tokens including 45 cached tokens, and 112 completion tokens. All ten raw
ledger rows recorded `routing_profile=beta` and `provider_id=blackbox`. Exact
provider-cost total was `$0.0007119000`; mean ledger latency was 3,484 ms.

The gate also proved:

- production credentials could not forge the beta profile;
- explicit price sorting selected the lower-priced OpenRouter mapping;
- rollback removed the Blackbox registry row and runtime secret reference;
- beta continued through OpenRouter during rollback; and
- restoration returned the next default beta request to Blackbox.

The final canary runtime had two Ready replicas in separate availability zones, zero
restarts, healthy PDB and EndpointSlices, successful keep-warm, health 200/200,
database availability metric `1`, and no serious router or AgentGateway log lines.

Issue #78 removed the superseded two-price layer and expanded the technically
validated Blackbox route to production in the checked registry. The comparative audit
in `docs/14` found DeepInfra had the strongest median price/throughput result, so both
profiles compile DeepInfra first, Blackbox second, and OpenRouter as the final
fallback. Blackbox remains enabled and eligible in both profiles; #78 retains the
deployed production canary and rollback proof.
