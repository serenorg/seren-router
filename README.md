<!-- ABOUTME: Top-level overview of seren-router, Seren's own OpenAI-compatible multi-provider LLM router.
ABOUTME: Links to the full design plans under docs/. -->

# seren-router

**Seren's own OpenAI-compatible multi-provider LLM aggregator — the service that replaces OpenRouter as the upstream behind the `seren-models` Gateway publisher.**

Today, `seren-models` (the publisher powering all public model chat in Seren Desktop and the Seren ecosystem) is a thin reverse proxy to OpenRouter: it forwards to `https://openrouter.ai/api/v1` with a Seren-owned static key and adds a 5% gateway fee. OpenRouter does the actual work of aggregating ~70 inference providers behind one OpenAI-compatible API.

With Stripe in talks to acquire OpenRouter (~$10B, WSJ, July 2026), that upstream becomes a critical dependency owned by a payments company that competes with SerenBucks. `seren-router` removes the dependency by rebuilding OpenRouter's core function — model-slug → provider selection → direct call with Seren's own keys → failover → unified cost accounting — as a service Seren owns end to end.

The cutover is deliberately invisible: because the whole Seren stack already speaks OpenRouter's dialect, we repoint one `api_url` field on the `seren-models` publisher from OpenRouter to seren-router and swap one key. Nothing downstream changes.

## Documents

| Doc | Contents |
| --- | --- |
| [`docs/00-overview.md`](docs/00-overview.md) | Strategic context, the Stripe/OpenRouter trigger, and the Shape A decision |
| [`docs/01-architecture.md`](docs/01-architecture.md) | The compatibility seam and the five internal components |
| [`docs/02-routing.md`](docs/02-routing.md) | Routing & failover — fastest-for-price default (Seren's own), with OpenRouter's algorithm offered as the `balanced` mode |
| [`docs/03-provider-registry.md`](docs/03-provider-registry.md) | Provider registry, key management, catalog sync |
| [`docs/04-billing.md`](docs/04-billing.md) | Cost-accounting contract and where margin grows |
| [`docs/05-migration.md`](docs/05-migration.md) | Gateway cutover and the incremental de-risked migration |
| [`docs/06-desktop-changes.md`](docs/06-desktop-changes.md) | The (small) seren-desktop client footprint |
| [`docs/07-infra-and-risks.md`](docs/07-infra-and-risks.md) | Repo, infrastructure, and honest risk register |
| [`docs/08-implementation-plan.md`](docs/08-implementation-plan.md) | Phased implementation plan |

## Status

Design phase. No implementation yet. These docs are the agreed design; the implementation plan in `docs/08` is the next actionable step.
