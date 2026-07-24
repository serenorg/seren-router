<!-- ABOUTME: Provider registry, key management, and catalog sync for seren-router.
ABOUTME: Defines how new providers/keys are added as a data operation, not a code deploy. -->

# 03 — Provider Registry, Keys & Catalog

The core requirement: **adding a new provider must be a data + secret operation, not a code deploy.** This is the "keep adding new API keys from new providers" mandate. The design makes that the common case and reserves code for genuinely non-standard hosts.

## Provider registry entry (declarative)

One config row per inference host, versioned in this repo:

```yaml
- id: fireworks
  display_name: Fireworks AI
  base_url: https://api.fireworks.ai/inference/v1
  auth:
    style: bearer            # Authorization: Bearer <key>
    secret_ref: SEREN_ROUTER_KEY_FIREWORKS   # name only — never the key
  adapter: openai-compatible # or a named shim for oddballs
  enabled: true
  overrides:                 # optional
    price_ceiling_usd_per_mtok: null
    weight: 1.0
    region: us
```

Key points:

- **`secret_ref` is a name, not a key.** The actual credential lives in the secrets manager (below). Rotating or adding a key never touches this file.
- **`adapter` defaults to `openai-compatible`.** Most of the chart's hosts (Together, Fireworks, DeepInfra, Novita, Baseten, Blackbox…) already speak OpenAI's API, so their adapters are thin. Only a host with a non-standard API needs a new code shim.

## Secrets store

Seren-owned provider keys live in the infrastructure's secrets manager, referenced by name from the registry. Adding or rotating a key is a secrets-manager write — no repo change, no redeploy. Keys never appear in the registry, logs, error payloads, or git.

## Catalog sync job

A scheduled task:

1. Hits each enabled provider's own `/models` endpoint (or a curated map where a provider lacks one).
2. Normalizes results into canonical `vendor/model` slugs.
3. Records that provider's price, context window, and capabilities per slug.
4. Rebuilds the **slug → providers index** that the router ranks over, and which backs `GET /api/v1/models`.

## Onboarding a provider — the happy path

1. Add a registry row.
2. Drop the key into the secrets manager.
3. Let the sync job discover its models.

No deploy for standard hosts. A non-standard host additionally needs an adapter (code).

## The honest hard part: slug normalization

Together, Fireworks, and Blackbox each name "Llama 3.3 70B" differently. Mapping all of them onto **one canonical slug** — so failover between providers for the same model actually works — is exactly the curation moat OpenRouter spent years building. We need:

- a **canonical-slug map** with per-provider aliases,
- partly hand-curated, partly derived from provider `/models` metadata,
- with a review step when the sync job encounters an unmapped provider model (surfaced, not silently dropped).

This is the single largest ongoing maintenance cost of owning the router. It is not free and should be staffed as real work, not assumed automatic.
