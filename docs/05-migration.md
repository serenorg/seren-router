<!-- ABOUTME: The Gateway cutover and incremental de-risked migration strategy for seren-router.
ABOUTME: Uses OpenRouter as a fallback provider during transition so the flip has zero coverage gaps. -->

# 05 — Gateway Cutover & Migration

## The literal switch

On the `seren-models` publisher, change two fields:

- `api_url`: `https://openrouter.ai/api/v1` → `https://<seren-router>/api/v1`
- the stored static key: Seren's OpenRouter key → a seren-router auth key

Everything else on the publisher stays byte-for-byte: endpoints, the pricing block, the 5% fee, `upstream_cost_response_path: usage.cost`, capabilities. It is a single `update_publisher` call. Because it is one field, **rollback is also one field.**

## The de-risking trick: OpenRouter as a fallback provider

A hard flip would put all of today's traffic (10k+ queries, 30 agents, all desktop chat) onto seren-router on day one. Instead:

**Register OpenRouter as just another provider inside seren-router's registry — the lowest-priority fallback.**

On day one, seren-router:

- serves whatever slugs it already has direct providers + keys for, and
- **falls back to OpenRouter for everything else.**

That makes seren-router an immediate *superset* of today's behavior. We can repoint `seren-models` early with **zero coverage gaps**, because anything not yet wired directly still resolves through OpenRouter underneath.

## Peel providers off incrementally

1. Validate DeepInfra Llama 3.3 through the credential-bound beta profile under
   the scoped credential, bounded-spend, and compatibility gates in `docs/11`.
2. Enable it only if compatibility, cost, and reliability remain green.
3. Re-evaluate Fireworks and Together after their documented no-go conditions
   change.
4. Validate Blackbox GLM 5.2 through the credential-bound internal beta profile.
   Keep production on OpenRouter while the standard-account commercial restriction
   and negative gross margin remain.
5. … each move shifts more traffic off OpenRouter onto direct connections.

When direct coverage reaches the models users actually call (checked against recent `seren-models` usage), OpenRouter's fallback share trends to zero — and we **delete that registry entry**. If the Stripe deal closes and OpenRouter's terms change mid-migration, we are already mostly off it.

## Rollout order

1. Stand up seren-router (with OpenRouter registered as fallback provider).
2. Coverage check: confirm the fallback path serves 100% of recent `seren-models` model slugs.
3. Canary: a small traffic slice — via a beta publisher slug or a percentage split — points at seren-router. Watch latency, error rate, cost parity.
4. Repoint `seren-models.api_url` to seren-router.
5. Peel providers off OpenRouter one at a time, monitoring each.
6. Remove the OpenRouter fallback entry when direct coverage is complete.

**Instant revert is available at every step** by restoring the `api_url` field.

## Pricing and cost gate

Before and during migration, confirm that seren-router reports the reviewed
route-independent sell subtotal at `usage.cost` for the same model and token counts.
Then compare the separately stored `provider_cost_usd` across routes. A direct
provider should ordinarily cost Seren less than the OpenRouter fallback while the
customer sell price remains exactly the catalog price. If it does not, leave that slug
on the production fallback; a bounded internal-beta route may still be useful for
resilience testing when its audience and spend are explicitly constrained.

## Other OpenRouter dependencies to absorb later

The publisher catalog contains at least one more OpenRouter-backed publisher — `google-gemini-3` ("via OpenRouter"). Out of scope for the initial migration, but it should eventually route through seren-router too. Track separately.
