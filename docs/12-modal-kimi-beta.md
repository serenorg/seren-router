<!-- ABOUTME: Defines the production and beta Kimi K3 route through Modal's Shared API.
ABOUTME: Pins neutral public naming, exact billing, credential scope, and activation gates. -->

# 12 — Modal Kimi K3 Route

## Decision and scope

Taariq approved authenticated internal beta validation and a cumulative $5 gross
provider-spend ceiling on 2026-07-29. On 2026-07-30, Taariq confirmed that Modal
granted Seren permission and approved both internal beta activation and a subsequent
bounded production validation.

The account-provisioned route is checked in as an enabled `production` and `beta`
route. Its first
bounded live run proved non-streaming JSON and cache telemetry, but Modal returned no
terminal usage object for the successful streaming request. The revised two-JSON gate
then passed with exact cached-token accounting. Streaming remains ineligible and
routes to OpenRouter. Compatible non-streaming Kimi K3 requests select Modal first in
both profiles and retain OpenRouter as their lowest-priority fallback. The production
publisher still targets seren-router, so publisher rollback remains an independent,
atomic restoration of the reviewed direct OpenRouter URL and credential.

Modal remains the immutable internal provider ID for routing, health, metrics, ledger
rows, and invoice reconciliation. Customer-facing endpoint and generation metadata use
the neutral `Seren Inference` / `seren` aliases. Public catalog serialization must not
contain the Modal brand, provider URL, credential name, or account-specific endpoint
identifier.

## Shared API contract

Kimi K3 is consumed through Modal's token-priced Shared API, not a dedicated Auto
Endpoint. Modal exposes two equivalent OpenAI-compatible forms:

- a direct endpoint URL with `Modal-Key` and `Modal-Secret` headers; and
- the regional inference gateway at
  `https://inference.us-west.modal.direct/v1`, authenticated with a standard bearer
  credential formed by joining the proxy-token ID and secret with one dot.

AgentGateway uses the regional form because its OpenAI provider already owns bearer
authentication. The registry's internal provider model ID is the exact hostname of
the account-provisioned Shared API endpoint. Requests still use the canonical public
slug `moonshotai/kimi-k3`; neither the endpoint hostname nor the internal provider ID
is accepted as a caller-selected model.

The registry declares this mapping compatible only with non-streaming Chat
Completions. An explicit numeric `top_p` must be within the inclusive interval
`0.95..=1`; absent and null values preserve the endpoint's default. Streaming, Legacy
Completions, and requests outside that interval remain on compatible beta routes. If
this compatibility check removes Modal, the selected and fallback routes both come
from the remaining concrete candidates. A non-null, nonnumeric `top_p` or `stream` is
rejected locally before any provider request.

An approved beta deployment exposes a newly reviewed proxy credential to AgentGateway
as `SEREN_ROUTER_KEY_MODAL` through the deployment secret manager and removes the
local validation credential afterward. The authenticated `serendb`
workspace is on Modal's Starter plan with RBAC disabled, so Modal reports the proxy
token as workspace-wide rather than environment-scoped. The rendered AgentGateway
config contains the environment reference, not its value. Only AgentGateway receives
the provider credential; the seren-router application receives the production and
optional beta Gateway credentials instead.

The Shared endpoint is provisioned from the authenticated dashboard's Managed flow.
The current `modal endpoint create` CLI command creates an Auto/Dedicated endpoint and
must not be substituted for this step. After creation, operations wait for the
dashboard endpoint record to publish its authoritative service URL and copy only that
URL's hostname into the internal provider-model mapping; the hostname is not derived
from the editable endpoint name.

## Exact economics

Modal's published Shared API rates are:

| Token class | Gross provider cost |
| --- | ---: |
| Uncached prompt | $3.00 / MTok |
| Cached prompt | $0.30 / MTok |
| Completion | $15.00 / MTok |
| Reasoning | $15.00 / MTok |

The router records gross provider cost before credits. Promotional-credit consumption
is reconciled separately from request-level cost. The authenticated
workspace displayed approximately $5,030 in credits on 2026-07-29. Taariq confirmed
on 2026-07-30 that this displayed balance is sufficient and that no reconciliation to
a separately reported promotional-grant amount is required. The independent
cumulative $5 validation ceiling remains the operative spend control.

Modal's OpenAI-compatible usage may report cache hits at
`usage.prompt_tokens_details.cached_tokens`. When that exact count is present and no
greater than total prompt tokens, the router applies the $0.30 cached-input rate to
those tokens. When it is absent or invalid, the router fails closed. It never treats
missing cache detail as zero or estimates provider cost.

## Beta validation gate

Live validation is manual, serial, and capped at a cumulative $5 gross provider spend.
Before any request:

1. Confirm the active CLI profile names the existing Seren workspace.
2. Confirm the current authenticated balance and compare it with the cumulative local
   reservation. The displayed balance is sufficient for the approved bounded
   validation; the CLI billing summary confirms credits applied during the cycle.
3. Provision or select the Kimi K3 Shared API endpoint through the dashboard's Managed
   flow and pin its authoritative service hostname in the internal registry mapping.
4. Create a proxy token, record whether the workspace can enforce environment scope,
   store its dot-joined value only in the approved secret boundary, and ensure it is
   not mounted into the router application.
5. Validate the compiled config with the pinned AgentGateway binary.

The revised paid gate sends the same deterministic prompt twice through non-streaming
JSON Chat Completions. It verifies exact provider cost, neutral public naming and
catalogs, profile-scoped generation lookup, and exact cached-token accounting. Its
hard cap reserves the uncached upper bound for the initial call
plus both configured sidecar retries for each logical request; failed attempts are
never assumed free. The harness reports logical request count, reserved attempt
ceiling, errors, latency, token totals, resolved gross provider cost, and reserved
gross-cost ceiling. Account credit consumption is reconciled
separately from the authenticated Credits and billing views. Any unresolved provider
cost on the cached repeat, public provider-brand or account-identifier leak, profile
leak, schema drift, or spend-ceiling violation blocks activation.

Both paid requests explicitly send `top_p: 0.95`, the lower reviewed boundary, so the
gate exercises the same request-compatibility contract used by beta routing.

A fresh billing-review UUID is consumed durably before each run. Before provider
access, the gate also writes the run's worst-case gross-cost reservation into the
operator-supplied `SEREN_MODAL_BILLING_STATE_DIR`. That absolute private directory
must live outside the repository and system temporary directory. Every retained
reservation counts against the cumulative $5 approval, including failed and timed-out
runs; reservations are never released automatically. Reconcile them against the
authenticated account billing view before changing or retiring that state.

Cache telemetry is not guaranteed by Modal's public contract, so the single repeat may
conservatively block an otherwise healthy endpoint; the gate does not issue an
unreviewed third paid probe. Before any rerun, operations must review current provider
billing, obtain explicit approval for the new paid run, and provide a new review UUID.

### First bounded run: blocked

The 2026-07-29 run reserved `$0.0740880000` of the cumulative approval before provider
access and sent exactly two logical requests. Modal's authenticated Activity view
reported:

| Request | Prompt | Cached prompt | Completion | Status |
| --- | ---: | ---: | ---: | --- |
| JSON | 1,902 | 64 | 4 | 200 |
| SSE | 1,902 | 1,856 | 4 | 200 |

Those counts reconcile exactly to `$0.00634800` of gross Shared API token cost:
`$0.00559320` for JSON plus `$0.00075480` for SSE. Modal also reported
`$0.00010454` of deployed-app cost, for `$0.00645254` total metered cost, all covered
by credits. The workspace balance displayed `$5,029.99` after the run.

The JSON response passed the router's response, neutral-branding, exact-cost, ledger,
and generation-metadata assertions. The successful SSE response contained `[DONE]`
but no terminal token-usage object, even though Modal's separate Activity view knew
the token counts. The router therefore did not invent `usage.cost` or write an
unreconciled generation row, and the activation gate failed as designed. No automatic
third request was sent.

Because Modal's public Shared API documentation does not promise terminal streaming
usage, the candidate now declares `supports_streaming: false`. A future enabled beta
configuration will route `stream: true` to OpenRouter directly.

Tool calling and strict structured output remain manual capability probes until their
deterministic activation fixtures are approved. Cross-provider failover remains
covered by the non-paid functional harness; the paid gate isolates Modal so its spend
and attribution cannot be confused with OpenRouter traffic.

### Revised JSON-only run: passed

After a fresh authenticated billing review and explicit operator approval, the second
2026-07-29 run retained another `$0.0740880000` worst-case reservation and sent exactly
two identical non-streaming JSON requests. The gate passed every assertion:

- 2 logical requests and no errors;
- 3,804 prompt tokens, including 1,920 exactly reported cached tokens;
- 8 completion tokens;
- `$0.0063480000` exact gross provider cost;
- 1,760.282 ms and 2,050.619 ms end-to-end request latency;
- exact internal provider attribution with neutral public catalog, response, and
  generation metadata; and
- production-profile exclusion plus exact beta ledger rows.

The authenticated Modal billing summary increased from `$0.00645254` to `$0.01280054`.
The `$0.00634800` delta was entirely LLM-token cost; deployed-app cost remained
`$0.00010454`, credits offset the full total, and billed cost remained `$0`. The two
retained reservations now total `$0.1481760000` against the approved cumulative `$5`
ceiling. They remain durable even though observed spend was lower.

This passing gate completed the provider integration contract. Taariq subsequently
confirmed permission and authorized the deployment secret, checked beta enablement,
and bounded production validation on 2026-07-30.

### Production canary: passed

The first ten-request production window returned ten successful responses but failed
the precommitted latency gate when its cold request took 22.147 seconds. The production
publisher was immediately and atomically restored to direct OpenRouter. Both rollback
JSON and SSE requests passed, including terminal streaming usage and `[DONE]`.

After one 1.114-second pre-warm through the isolated beta publisher, production was
atomically restored to seren-router and a fresh ten-request window passed without
changing the threshold:

- 10 requests, 10 HTTP 200 responses, and no provider or request errors;
- 3.130-second p95 latency against the 15-second ceiling;
- 30,420 prompt tokens, including 30,080 cached tokens, and 40 completion tokens;
- `$0.0106440000` exact gross provider cost;
- ten production ledger rows attributed internally to `modal` and exposed publicly as
  `Seren Inference`; and
- an exact Modal billing increase from `$0.04689314` to `$0.05753714`, with billed
  cost remaining `$0`.

Streaming, Legacy Completions, and `top_p=0.949` probes each selected OpenRouter,
returned HTTP 200, and wrote OpenRouter-attributed production ledger rows. Modal
billing did not change during those probes. Both router replicas remained Ready in
separate zones with zero restarts, zero CPU throttling, healthy PDB and EndpointSlice
state, and successful public health checks. The production publisher finished on
seren-router; its tested atomic direct-OpenRouter restoration remains the immediate
publisher rollback.

## Rollout and rollback

The approved deployment uses distinct production and beta Gateway credentials and one
Modal credential held by the deployment secret manager. Production admission is a
bounded canary with ten controlled JSON requests, zero request/provider errors, p95
latency at or below 15 seconds, exact reconciliation of reported cached-token provider
cost, no readiness loss or restart, and 100% OpenRouter selection for incompatible
streaming, Legacy Completions, and out-of-range `top_p` probes. Rollback has two
independent layers:

1. atomically restore the production publisher to the reviewed direct OpenRouter URL
   and encrypted credential; and
2. remove Modal from the production profile, regenerate AgentGateway config, and roll
   back the resource-scoped deployment if the route itself must be disabled.

The beta credential and route remain intact during a production-only rollback.

## Primary evidence

- [Modal Kimi K3 model page](https://modal.com/library/moonshot/kimi-k3)
- [Modal Kimi K3 launch note](https://modal.com/blog/kimi-k3-by-moonshot-now-available-on-modal)
- [Modal billing documentation](https://modal.com/docs/guide/billing)
- [Modal endpoint documentation](https://modal.com/docs/guide/endpoints)
