<!-- ABOUTME: Evidence and activation gates for the first direct provider.
ABOUTME: Selects a disabled DeepInfra Llama beta route without installing credentials or moving traffic. -->

# 11 — First Direct Provider Decision

Decision date: 2026-07-27.

## Decision

Select **DeepInfra `meta-llama/Llama-3.3-70B-Instruct-Turbo`** as the first
engineering-canary pair for the canonical
`meta-llama/llama-3.3-70b-instruct` slug.

The checked-in provider remains:

- disabled;
- restricted to the `beta` routing profile;
- without a credential; and
- unable to receive production traffic.

This is an engineering-canary selection, not a demand-ranked rollout decision.
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

| Route | Input / MTok | Output / MTok | 72-hour provider cost | 5% Gateway fee | Customer total |
| --- | ---: | ---: | ---: | ---: | ---: |
| OpenRouter fallback | $0.13 | $0.40 | $0.0006006600 | $0.0000300330 | $0.0006306930 |
| DeepInfra direct | $0.10 | $0.32 | $0.0004646800 | $0.0000232340 | $0.0004879140 |

For this token mix, the direct route is $0.0001359800 cheaper, a
22.6384310592% provider-cost reduction. Input is 23.0769230769% cheaper and
output is 20% cheaper.

The current true-cost-plus-5% billing contract passes the saving to the customer
and reduces the absolute Gateway fee. It does **not** retain the removed
middleman spread as Seren margin. That pricing-policy mismatch must be resolved
before presenting the provider move as margin expansion.

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
with an approved region is contracted. Subprocessor and deletion commitments
must be confirmed in the executed commercial agreement rather than inferred
from the public page.

The account prerequisites are an executed consent/DPA acceptable to Seren,
funded billing, an organization-owned account, and a model-scoped service key.
DeepInfra does not publish one fixed numerical RPM/TPM allowance for this route;
the account API returns the assigned limits, so recording those live values is
an activation gate. The reviewed public privacy and terms pages do not provide
a sufficient subprocessor schedule or a contractual deletion SLA. Activation
therefore remains blocked until those terms are supplied and approved in
writing.

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

Selection does not authorize activation. Every item below must be satisfied in
a separate issue before the provider is enabled:

1. Obtain written DeepInfra consent for Seren's customer-facing routed service
   and comparative compatibility/reliability testing. The public
   [terms](https://deepinfra.com/terms) require prior written consent for use
   directly or indirectly competitive with DeepInfra.
2. Name Taariq Lewis as account owner and Platform/Security as runtime-secret
   custodian. Store only a model-scoped, spend-limited credential in the
   approved production secret manager.
3. Set credential expiry to at most 90 days; rotate before expiry and
   immediately on suspected exposure or owner/access changes.
4. Start with a cumulative $5 beta spend limit and record the account's actual
   RPM/TPM limits before sending traffic.
5. Resolve the true-cost billing/margin policy and approve the customer price.
6. Run JSON, SSE, schema, usage-cost, provider-attribution, failover, and
   zero-cross-profile-leakage tests through the beta credential only.
7. Keep OpenRouter enabled for both profiles as the immediate fallback and do
   not alter the production publisher during the beta.
