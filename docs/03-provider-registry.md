<!-- ABOUTME: Provider registry, key management, and catalog sync for seren-router.
ABOUTME: Defines how new providers/keys are added as a data operation, not a code deploy. -->

# 03 — Provider Registry, Keys & Catalog

The core requirement: **adding a new provider must be a data + secret operation, not a code deploy.** This is the "keep adding new API keys from new providers" mandate. The design makes that the common case and reserves code for genuinely non-standard hosts.

## Provider registry entry (declarative)

Canonical sell prices and one config row per inference host are versioned in this
repo:

```yaml
sell_prices:
  - slug: meta-llama/llama-3.3-70b-instruct
    input_price_per_mtok: "0.13"
    output_price_per_mtok: "0.40"

providers:
  - id: deepinfra
    display_name: DeepInfra
    base_url: https://api.deepinfra.com/v1/openai
    secret_env: SEREN_ROUTER_KEY_DEEPINFRA # name only — never the key
    enabled: false
    priority: 0
    profiles: [beta]
    models:
      - slug: meta-llama/llama-3.3-70b-instruct
        name: Meta Llama 3.3 70B Instruct
        context_length: 131072
        provider_model_id: meta-llama/Llama-3.3-70B-Instruct-Turbo
        input_price_per_mtok: "0.10"  # exact provider cost
        output_price_per_mtok: "0.32"
```

Key points:

- **`secret_env` is a name, not a key.** The actual credential lives in the
  secrets manager (below). Rotating or adding a key never puts it in this file.
- **Provider prices are cost, not customer price.** The top-level `sell_prices`
  table owns the route-independent customer subtotal reported at `usage.cost`.
  See `docs/04`.
- **OpenAI-compatible is the current standard path.** Most candidate hosts already
  speak OpenAI's API. Only a host with a non-standard API needs a new code shim.
- **`profiles` is an allowlist, not a traffic hint.** Omission safely defaults to
  production for backward compatibility. A beta-only provider must declare
  `profiles: [beta]`; routing, catalog responses, measurements, share state, and
  AgentGateway failover aliases all enforce the same boundary.

## Credential-bound production and beta profiles

`SEREN_ROUTER_GATEWAY_KEY` always authenticates the production profile. An optional,
distinct `SEREN_ROUTER_BETA_GATEWAY_KEY` authenticates beta. Empty or identical keys
are rejected during configuration. The middleware attaches the profile as an internal
request extension after a constant-time credential comparison; HTTP headers cannot
override it.

This permits one deployed router and one sidecar to validate a new direct provider
without making its key or route reachable from the production publisher. Enabling a
beta provider still requires the provider credential to be present in the sidecar, but
the production credential cannot select it directly, discover it in catalog endpoints,
or reach it during failover. Successful generation metadata is stored with the
credential-selected profile, so knowing a beta response ID does not make
`/api/v1/generation` readable with production credentials (or vice versa).

### Isolation threat model

The boundary assumes a caller may know every public model slug, guess a generation ID,
forge an `x-seren-routing-profile` header, or send OpenRouter-style
`provider.only`, `provider.ignore`, or `provider.order` overrides. None of those inputs
selects a profile. Authentication chooses the profile internally; direct provider
overrides are rejected before routing; catalog snapshots, routing candidates, rolling
share state, live measurements, sidecar failover aliases, generation rows, logs, and
metrics remain profile-scoped.

This boundary does not claim that beta provider credentials are absent from the pod.
They are available only to AgentGateway targets that the generated beta alias can
reference. Generated config validation against the pinned AgentGateway binary is
therefore a release gate.

Rollback is configuration-only: remove `SEREN_ROUTER_BETA_GATEWAY_KEY`, disable the
beta-only provider registry rows, regenerate the sidecar config, and roll back the
Deployment. Production provider rows, production aliases, and the incumbent OpenRouter
fallback are not edited to conduct a beta trial.

## Secrets store

Seren-owned provider keys live in the infrastructure's secrets manager, referenced by name from the registry. Adding or rotating a key is a secrets-manager write — no repo change, no redeploy. Keys never appear in the registry, logs, error payloads, or git.

## Catalog sync job

A scheduled task:

1. Hits each enabled provider's own `/models` endpoint (or a curated map where a provider lacks one).
2. Normalizes results into canonical `vendor/model` slugs.
3. Records that provider's cost, context window, and capabilities per slug.
4. Rebuilds the **slug → providers index** that the router ranks over, and which backs `GET /api/v1/models`.

Discovery never changes `sell_prices`. A new slug or customer-price change needs
explicit owner review. Catalog `pricing` fields always come from the reviewed sell
table, so provider discovery cannot silently alter customer billing.

## Onboarding a provider — the happy path

1. Add or review the canonical sell-price row.
2. Add a provider registry row with exact provider costs.
3. Drop the key into the secrets manager.
4. Let the sync job discover its models.

No deploy for standard hosts. A non-standard host additionally needs an adapter (code).

## The honest hard part: slug normalization

Together, Fireworks, and Blackbox each name "Llama 3.3 70B" differently. Mapping all of them onto **one canonical slug** — so failover between providers for the same model actually works — is exactly the curation moat OpenRouter spent years building. We need:

- a **canonical-slug map** with per-provider aliases,
- partly hand-curated, partly derived from provider `/models` metadata,
- with a review step when the sync job encounters an unmapped provider model (surfaced, not silently dropped).

This is the single largest ongoing maintenance cost of owning the router. It is not free and should be staffed as real work, not assumed automatic.
