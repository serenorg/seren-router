<!-- ABOUTME: Evidence and activation record for the first direct provider.
ABOUTME: Enables DeepInfra Llama only for the credential-bound beta profile. -->

# 11 — First Direct Provider Decision

Decision date: 2026-07-27. Activation record updated 2026-07-30.

## Decision

Select **DeepInfra `meta-llama/Llama-3.3-70B-Instruct-Turbo`** as the first
engineering-canary pair for the canonical
`meta-llama/llama-3.3-70b-instruct` slug.

The checked-in provider is:

- enabled;
- restricted to the `beta` routing profile;
- unable to receive production traffic.

Its runtime credential is restricted to the selected Llama model, expires after
30 days, and has a $5 lifetime spending limit. It exists only in the approved secret
boundary and is not committed or printed.

This remains an engineering-canary route, not a demand-ranked production rollout.
The prompt-free 72-hour production ledger window ending 2026-07-27 contained
211 successful requests, all for the Llama canonical slug. Those requests were
validation traffic and are not representative enough to rank the wider model
catalog by organic demand.

## Aggregate evidence

The 72-hour aggregate contained no prompts, authorization data, customer
identifiers, or generation bodies:

| Canonical slug | Requests | Prompt tokens | Completion tokens | Cost | Avg latency | p95 latency | Failures |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `meta-llama/llama-3.3-70b-instruct` | 211 | 3,962 | 214 | $0.0006006600 | 1,048.943 ms | 7,090.5 ms | 0 |

At the reviewed prices, exact decimal arithmetic gives:

| Route | Provider input / MTok | Provider output / MTok | Provider cost | Sell subtotal | 5% Gateway fee | Customer total |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| OpenRouter fallback | $0.13 | $0.40 | $0.0006006600 | $0.0006006600 | $0.0000300330 | $0.0006306930 |
| DeepInfra direct | $0.10 | $0.32 | $0.0004646800 | $0.0006006600 | $0.0000300330 | $0.0006306930 |

For this token mix, the direct route is $0.0001359800 cheaper, a
22.6384310592% provider-cost reduction. Input is 23.0769230769% cheaper and
output is 20% cheaper.

The approved policy in `docs/04` keeps one reviewed sell price across routes and
stores provider cost separately. For this mix, the direct route therefore creates
$0.0001359800 of router gross margin, or 22.6384310592% of the sell subtotal,
without changing the documented customer price or the existing Gateway fee.

## Compatibility and operations

DeepInfra is the best technical canary among the reviewed candidates:

- The [model page](https://deepinfra.com/meta-llama/Llama-3.3-70B-Instruct-Turbo)
  identifies the 131,072-token Turbo model and its JSON/function support.
- [Published pricing](https://deepinfra.com/pricing) is $0.10 input and $0.32
  output per million tokens.
- The [OpenAI-compatible API](https://docs.deepinfra.com/chat/overview) accepts
  chat completions at `https://api.deepinfra.com/v1/openai`.
- [Streaming](https://docs.deepinfra.com/chat/streaming) uses SSE, includes
  `prompt_tokens` and `completion_tokens` in the terminal event, and terminates
  with `[DONE]`.
- [Rate limits](https://docs.deepinfra.com/api-reference/account/account-rate-limit)
  expose per-model outstanding-request and token-per-minute limits. Account
  values must be captured after provisioning and before load testing.
- [Scoped credentials](https://docs.deepinfra.com/account/authentication) can
  constrain the model, expiry, and USD spend.
- [Data handling](https://docs.deepinfra.com/account/data-privacy) says ordinary
  inference inputs and outputs are held in memory, are not used for training,
  and are generally not content-logged. It reserves limited content logging for
  debugging or security. Google and Anthropic routes have separate exceptions;
  they are not part of this canary.

No documented region selector was found for public serverless inference.
Region-pinned workloads therefore remain out of scope unless a private endpoint
with an approved region is contracted. That does not block this public-serverless
beta route.

DeepInfra's written public [Terms](https://deepinfra.com/terms) define its APIs as
Services and expressly permit use of the Services for legal commercial purposes
unless a prohibited-use clause applies. Seren accepted those terms during organization
account signup and recorded its intended internal-beta and future customer-facing
routing use. No separate private permission instrument is required for this beta.

The authenticated account API reports an assigned concurrent-request limit of `200`
and a token-per-minute limit of `1,100,000`. The organization-owned account is held by
Taariq Lewis. On 2026-07-30 it issued a scoped JWT restricted to
`meta-llama/Llama-3.3-70B-Instruct-Turbo`, with a `$5.00` lifetime spending limit and
expiry at `2026-08-29T00:49:43Z`. The credential is stored in the approved private
secret boundary and its value is absent from source, rendered configuration, logs,
and evidence.

## Live activation evidence

The funded gate passed on 2026-07-30 through the credential-bound beta route.
Two direct preflight requests proved JSON and SSE compatibility before deployment:
both returned 15 prompt tokens, 2 completion tokens, terminal usage, and exact
`$0.00000214` provider cost; SSE emitted exactly one `[DONE]`.

The deployed canary then completed 10 valid requests: 5 JSON and 5 SSE, with zero
errors. The requests used 169 prompt and 47 completion tokens, averaged
1,174.260 ms, and recorded `$0.0000319400` exact provider cost against
`$0.0000407700` route-independent sell cost. Every response selected DeepInfra,
and every generation row recorded `provider_name = deepinfra`. The SSE responses
all included terminal usage and exactly one `[DONE]`.

The authenticated catalogs remained profile-isolated: production exposed only
OpenRouter for the Llama slug, while beta exposed DeepInfra first and OpenRouter
second. Both advertised the invariant `$0.13` input and `$0.40` output sell
prices per million tokens. A production credential carrying both a forged
`x-seren-routing-profile: beta` header and a forged request-body profile still
selected OpenRouter.

Rollback was exercised at the immutable previous GitOps revision. It disabled
the DeepInfra registry row, removed the runtime secret reference, kept both
replicas healthy, and caused the beta publisher to serve the same slug through
OpenRouter. Restoring the approved revision returned the next beta generation
to DeepInfra at 382 ms. The final deployment has two Ready replicas in separate
availability zones, zero restarts, healthy PDB and EndpointSlices, successful
keep-warm execution, 200 liveness/readiness, and no serious router or
AgentGateway log lines.

## Candidate no-go decisions

### Together — no-go for this canary

Together exposes the right Llama family and OpenAI-compatible surface, but its
[published price](https://www.together.ai/models/llama-3-3-70b) is $1.04 input
and $1.04 output per million tokens. That is materially more expensive than the
current OpenRouter fallback. Its
[Terms of Service](https://www.together.ai/terms-of-service) also prohibit
standalone resale and competitive benchmarking without an applicable written
agreement. The old roadmap preference for Together is superseded.

### Fireworks — no-go for this canary

The [Fireworks Llama 3.3 model page](https://fireworks.ai/models/fireworks/llama-v3p3-70b-instruct)
marks serverless inference unsupported, so it cannot serve as the bounded,
pay-per-token beta route. Its
[Terms of Service](https://fireworks.ai/terms-of-service) also restrict
competitive use and benchmarking. The empty Fireworks registry placeholder is
removed rather than implying an approved route.

## Activation gates

Issue #59 owns the following activation record:

1. **Satisfied:** DeepInfra's written public Terms authorize legal commercial use;
   Seren accepted them while recording the intended routed-inference use.
2. **Satisfied:** Taariq Lewis is account owner; Platform/Security is runtime-secret
   custodian. Only the model-scoped credential may enter the deployment secret.
3. **Satisfied:** credential expiry is 30 days, below the 90-day maximum. Rotate
   before expiry and immediately on suspected exposure or owner/access changes.
4. **Satisfied:** the credential's lifetime spending limit is $5; live account limits
   are 200 concurrent requests and 1.1M TPM.
5. **Resolved in #58:** keep the reviewed OpenRouter-equivalent sell price,
   preserve exact provider cost separately, and let Gateway apply its 5% once.
6. **Satisfied:** JSON, SSE, terminal usage, exact provider/sell cost,
   provider attribution, profile isolation, bounded canary, failover, and
   rollback/restore passed through the beta credential.
7. **Enforced in registry:** OpenRouter stays enabled for both profiles as the
   immediate fallback; the production publisher is not altered by this beta.
