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
| [`docs/09-agentgateway-evaluation.md`](docs/09-agentgateway-evaluation.md) | DECIDED: built on agentgateway (Linux Foundation, Rust, Apache-2.0) — verified by source inspection + live functional test |
| [`docs/plans/20260724_plan_seren_router_build.md`](docs/plans/20260724_plan_seren_router_build.md) | The build plan: zero-context, task-by-task implementation guide (M0–M6) with tests and commit points |

## Status

Implementation is underway. The service uses the standard Seren Rust chassis and is
built as a **thin OpenRouter-compatibility, pricing-policy, and cost-accounting layer on
[agentgateway](https://github.com/agentgateway/agentgateway)** (see `docs/09` for the
verified evaluation).

## Service development

Rust 1.95 or newer and PostgreSQL 17 are required. The production feature group enables
metrics, security headers, sensitive-header redaction, and payload limits; it
deliberately excludes the template's per-IP rate limiter because the Gateway is this
service's single caller. `DATABASE_URL` is required for tests and runtime startup.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --features production -- -D warnings
cargo test --features production
```

Run the chassis locally:

```bash
DATABASE_URL=postgresql://postgres:postgres@localhost:5432/seren_router \
SEREN_ROUTER_GATEWAY_KEY=local-development-only \
cargo run --features production
curl http://127.0.0.1:8000/readyz
# {"status":"ok"}
```

`SEREN_ROUTER_GATEWAY_KEY` and `DATABASE_URL` are required. Startup connects to
PostgreSQL and applies embedded migrations before the HTTP listener binds; `/readyz`
continues checking that pool. The sidecar URL defaults to `http://127.0.0.1:4000`, and
the provider registry path defaults to `registry/providers.yaml`; override them with
`SEREN_ROUTER_SIDECAR_URL` and `SEREN_ROUTER_REGISTRY_PATH`.

The protected route registry exposes the initial completion and generation paths:

- `POST /api/v1/chat/completions`
- `POST /api/v1/completions`
- `GET /api/v1/generation?id=<provider-response-id>`

All require `Authorization: Bearer <SEREN_ROUTER_GATEWAY_KEY>`. Completion responses
carry exact provider `usage.cost`; successful costed generations are persisted for
post-hoc lookup and reconciliation.

## Pinned agentgateway sidecar

The stock agentgateway binary is downloaded from its official GitHub release and kept
out of Git. The installer supports Darwin arm64, Linux amd64, and Linux arm64; it
verifies the platform-specific SHA-256 digest before replacing an installed binary.

```bash
./scripts/fetch-sidecar.sh
./sidecar/bin/agentgateway --version
```

The version and all supported-platform digests are reviewable in
`sidecar/PINNED_VERSION`.

## Copyright

Copyright (c) 2026 SerenAI. All rights reserved. See [LICENSE](LICENSE).
