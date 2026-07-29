<!-- ABOUTME: Defines the beta-only Kimi K3 route through Modal's Shared API.
ABOUTME: Pins neutral public naming, exact billing, credential scope, and activation gates. -->

# 12 — Modal Kimi K3 Beta Route

## Decision and scope

Taariq approved authenticated internal beta validation and a cumulative $5 gross
provider-spend ceiling on 2026-07-29. That approval covered account authentication,
endpoint provisioning, and bounded internal validation; it did not establish written
permission for customer-serving use. Retain that permission as an independent rollout
gate.

The account-provisioned route is checked in as a disabled `beta` candidate. Its first
bounded live run proved non-streaming JSON and cache telemetry, but Modal returned no
terminal usage object for the successful streaming request. The router therefore
blocked activation. Production and beta both remain OpenRouter-only; no deployment
secret, customer traffic, or publisher changed.

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

The joined proxy credential is retained only in the local macOS Keychain for
validation. An approved beta deployment would expose it to AgentGateway as
`SEREN_ROUTER_KEY_MODAL` through the deployment secret manager and remove the local
copy afterward. No deployment secret has been created. The authenticated `serendb`
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

The existing canonical Kimi sell price remains $3.00 input and $15.00 output per
million tokens. No customer price changes when the beta route is added.

The router records gross provider cost before credits. Promotional-credit consumption
is reconciled separately from request-level cost and margin. The authenticated
workspace displayed a $5,030 credit balance on 2026-07-29, not the operator-reported
$25,000. The beta gate relies only on the separately approved cumulative $5 ceiling;
the larger grant amount, expiry, and restrictions remain unverified and must not be
used for production planning.

Modal's OpenAI-compatible usage may report cache hits at
`usage.prompt_tokens_details.cached_tokens`. When that exact count is present and no
greater than total prompt tokens, the router applies the $0.30 cached-input rate to
those tokens. When it is absent or invalid, the router still records and returns the
exact reviewed sell subtotal but persists `provider_cost_usd` as null. It never treats
missing cache detail as zero or estimates gross provider cost.

## Beta validation gate

Live validation is manual, serial, and capped at a cumulative $5 gross provider spend.
Before any request:

1. Confirm the active CLI profile names the existing Seren workspace.
2. Confirm the current authenticated balance and compare it with the cumulative local
   reservation. Record any grant expiry or restrictions if the Credits view exposes
   them, without copying confidential billing data into this repository. The CLI
   billing summary can confirm credits applied during a cycle but cannot establish
   remaining grant eligibility.
3. Provision or select the Kimi K3 Shared API endpoint through the dashboard's Managed
   flow and pin its authoritative service hostname in the internal registry mapping.
4. Create a proxy token, record whether the workspace can enforce environment scope,
   store its dot-joined value only in the approved secret boundary, and ensure it is
   not mounted into the router application.
5. Validate the compiled config with the pinned AgentGateway binary.

The revised paid gate sends the same deterministic prompt twice through non-streaming
JSON Chat Completions. It verifies invariant customer sell price, neutral public
naming and catalogs, beta-only generation lookup, the correct null provider cost when
a cold response omits cache detail, and exact cached-token provider cost on the
repeated request. Its hard cap reserves the uncached upper bound for the initial call
plus both configured sidecar retries for each logical request; failed attempts are
never assumed free. The harness reports logical request count, reserved attempt
ceiling, errors, latency, token totals, resolved gross provider cost, reserved
gross-cost ceiling, and sell subtotal. Account credit consumption is reconciled
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
configuration will route `stream: true` to OpenRouter directly. The candidate remains
disabled until the revised two-JSON gate passes.

Tool calling and strict structured output remain manual capability probes until their
deterministic activation fixtures are approved. Cross-provider failover remains
covered by the non-paid functional harness; the paid gate isolates Modal so its spend
and attribution cannot be confused with OpenRouter traffic.

## Rollout and rollback

A passing revised test permits consideration of a beta deployment. Enabling the
candidate, copying its credential into the deployment secret manager, and adding a
beta Gateway credential require a separate deployment approval. Moving the route into
the production profile additionally requires written customer-serving permission and
a separate production rollout decision. Until those gates pass, production and beta
traffic remain on OpenRouter. Rollback is configuration-only:

1. remove `SEREN_ROUTER_BETA_GATEWAY_KEY`;
2. disable the Modal registry row;
3. regenerate AgentGateway config and roll back the deployment; and
4. revoke the Modal proxy token after traffic is confirmed at zero.

No production publisher or OpenRouter mapping is edited for this trial.

## Primary evidence

- [Modal Kimi K3 model page](https://modal.com/library/moonshot/kimi-k3)
- [Modal Kimi K3 launch note](https://modal.com/blog/kimi-k3-by-moonshot-now-available-on-modal)
- [Modal billing documentation](https://modal.com/docs/guide/billing)
- [Modal endpoint documentation](https://modal.com/docs/guide/endpoints)
