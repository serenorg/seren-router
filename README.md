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
| [`docs/04-billing.md`](docs/04-billing.md) | Exact served-provider cost accounting and Gateway fee boundary |
| [`docs/05-migration.md`](docs/05-migration.md) | Gateway cutover and the incremental de-risked migration |
| [`docs/06-desktop-changes.md`](docs/06-desktop-changes.md) | The (small) seren-desktop client footprint |
| [`docs/07-infra-and-risks.md`](docs/07-infra-and-risks.md) | Repo, infrastructure, and honest risk register |
| [`docs/08-implementation-plan.md`](docs/08-implementation-plan.md) | Phased implementation plan |
| [`docs/09-agentgateway-evaluation.md`](docs/09-agentgateway-evaluation.md) | DECIDED: built on agentgateway (Linux Foundation, Rust, Apache-2.0) — verified by source inspection + live functional test |
| [`docs/10-deployment.md`](docs/10-deployment.md) | Validated two-container pod boundary, image pin, probes, and local production smoke |
| [`docs/11-first-direct-provider.md`](docs/11-first-direct-provider.md) | First direct-provider selection, evidence, and activation gates |
| [`docs/12`](docs/12-modal-kimi-beta.md) | Kimi K3 beta inference contract, neutral naming, and activation gates |
| [`docs/13`](docs/13-blackbox-glm-beta.md) | Blackbox GLM 5.2 internal-beta contract, metered pricing, and rollout boundary |
| [`docs/14`](docs/14-glm52-provider-audit.md) | GLM 5.2 provider price and measured throughput audit |
| [`docs/plans/20260724_plan_seren_router_build.md`](docs/plans/20260724_plan_seren_router_build.md) | The build plan: zero-context, task-by-task implementation guide (M0–M7) with tests and commit points |

## Status

The Phase 1 compatibility skeleton and live M6 OpenRouter parity/soak gate are complete
as of 2026-07-25. The service uses the standard Seren Rust chassis and is built as a
**thin OpenRouter-compatibility, routing-policy, and cost-accounting layer on
[agentgateway](https://github.com/agentgateway/agentgateway)** (see `docs/09` for the
verified evaluation). The OpenRouter-only image was deployed to the existing production
environment and passed its bounded canary on 2026-07-26. The repository also carries
a credential-isolated Kimi K3 route. Its revised
JSON-only live gate passed on 2026-07-29 with exact cached-token accounting; streaming
remains ineligible because the first bounded run omitted terminal streaming usage.
Taariq confirmed Modal permission and approved beta activation and a bounded production
canary on 2026-07-30. The production gate passed after exercising the atomic
direct-OpenRouter publisher rollback and an isolated beta pre-warm. The checked route
is enabled for production and credential-bound beta traffic. Compatible non-streaming
Kimi K3 requests select Modal first, while OpenRouter remains the lowest-priority
fallback in both profiles. The checked registry now enables DeepInfra Llama 3.3 70B,
DeepInfra GLM 5.2, and Blackbox GLM 5.2 for production and credential-bound beta
traffic. The earlier DeepInfra Llama and Blackbox beta canaries passed exact
provider-cost, compatibility, failover, and rollback/restore gates. A subsequent
five-request-per-provider GLM audit measured DeepInfra as the best stable
price/throughput route, so the checked default GLM order is DeepInfra, Blackbox, then
OpenRouter; explicit price sorting selects OpenRouter. The revised production gate
passed on 2026-07-30 across both routing profiles and both JSON/SSE modes, with exact
response, generation-metadata, and raw-ledger provider-cost identity. Two selective
rollback exercises proved OpenRouter continuity before the final healthy restore.

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
# Terminal 1: render and start the pinned sidecar.
./scripts/fetch-sidecar.sh
cargo run --features production -- \
  render-sidecar-config /tmp/seren-router-agentgateway.yaml
SEREN_ROUTER_KEY_OPENROUTER=<from-secret-manager> \
  ./sidecar/bin/agentgateway -f /tmp/seren-router-agentgateway.yaml

# Terminal 2: start the app and query inference readiness.
DATABASE_URL=postgresql://postgres:postgres@localhost:5432/seren_router \
SEREN_ROUTER_GATEWAY_KEY=local-development-only \
SEREN_ROUTER_COMBINED_PRICE_CEILING_PER_MTOK=<reviewed-combined-price-ceiling> \
SEREN_ROUTER_HYSTERESIS_FRACTION=<reviewed-fraction> \
SEREN_ROUTER_MAX_SHARE=<reviewed-fraction> \
SEREN_ROUTER_SHARE_WINDOW=<reviewed-request-count> \
cargo run --features production
curl http://127.0.0.1:8000/readyz
# {"status":"ok","dependencies":{"database":"ok","sidecar":"ok"}}
```

`SEREN_ROUTER_GATEWAY_KEY`, `DATABASE_URL`, and the four routing-policy variables shown
above are required. The router deliberately has no unreviewed production defaults for
the price ceiling, hysteresis, provider max share, or rolling share-window size.
The AgentGateway sidecar requires `SEREN_ROUTER_KEY_OPENROUTER` while the checked-in
OpenRouter fallback provider is enabled. The enabled direct routes also require
`SEREN_ROUTER_KEY_MODAL`, `SEREN_ROUTER_KEY_DEEPINFRA`,
`SEREN_ROUTER_KEY_DEEPINFRA_GLM`, and `SEREN_ROUTER_KEY_BLACKBOX`. Inject all provider
credentials from the deployment secret manager into AgentGateway only; never store
their values in the registry, rendered configuration, or seren-router app container.
Startup opens a lazy PostgreSQL pool, binds the HTTP listener without waiting for the
generation ledger, and retries embedded migrations in an event-driven background
supervisor. `/readyz` requires AgentGateway and reports PostgreSQL as `starting`, `ok`,
or `degraded` without taking inference out of service. `/livez` remains dependency-free.
The supervisor does not periodically poll PostgreSQL, so it does not keep scale-to-zero
compute awake. The sidecar URLs default to `http://127.0.0.1:4000` and
`http://127.0.0.1:19001/healthz/ready`; the provider registry path defaults to
`registry/providers.yaml`. Override them with `SEREN_ROUTER_SIDECAR_URL`,
`SEREN_ROUTER_SIDECAR_READINESS_URL`, and `SEREN_ROUTER_REGISTRY_PATH`.

The protected route registry exposes completion, generation, catalog, and compatibility
paths:

- `POST /api/v1/chat/completions`
- `POST /api/v1/completions`
- `GET /api/v1/generation?id=<provider-response-id>`
- `GET /api/v1/models`
- `GET /api/v1/models/<author>/<slug>/endpoints`
- `GET /api/v1/models/<percent-encoded-model>/endpoints`
- `GET /api/v1/auth/key`
- `GET /api/v1/credits`

All require a Gateway bearer credential. `SEREN_ROUTER_GATEWAY_KEY` is bound to the
production provider profile; optional `SEREN_ROUTER_BETA_GATEWAY_KEY` is bound to the
beta profile and must be distinct. Client headers cannot choose or override the
profile. Completion responses carry the exact serving-provider cost at `usage.cost`;
successful generations persist that same amount. When no trustworthy upstream cost is
present, a cached-price route fails closed if the upstream omits usage detail required
for an exact registry-rate calculation. The model and endpoint catalogs are assembled at
startup from enabled provider mappings and report exact per-token prices as decimal
strings. Endpoint responses expose registry metadata only; they never expose provider
URLs or secret names. The key and credit responses are fixed compatibility metadata
because account billing remains owned by the Gateway.

The paid OpenRouter parity and 100-request soak gates are manual and opt-in. Their
credential, explicit spend ceiling, commands, metrics, and cleanup contract are
documented in [`tests/functional/README.md`](tests/functional/README.md).

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

Production uses the official AgentGateway image by immutable OCI index digest.
`sidecar/PINNED_IMAGE` records the index and Linux platform manifests, and
`./scripts/verify-sidecar-image.sh` checks them against GHCR. The app image can render
the mounted registry for an init container without any runtime secrets:

```bash
SEREN_ROUTER_REGISTRY_PATH=registry/providers.yaml \
  ./target/release/seren-router render-sidecar-config /config/agentgateway.yaml
```

The cluster-neutral pod boundary and local container smoke are documented in
[`docs/10-deployment.md`](docs/10-deployment.md).

## Copyright

Copyright (c) 2026 SerenAI. All rights reserved. See [LICENSE](LICENSE).
