<!-- ABOUTME: Technical activation record for the first direct provider. -->
<!-- ABOUTME: Records DeepInfra Llama compatibility, cost, failover, and rollback evidence. -->

# 11 — First Direct Provider Decision

Decision date: 2026-07-27. Activation record updated 2026-07-30.

## Route

DeepInfra `meta-llama/Llama-3.3-70B-Instruct-Turbo` serves the canonical
`meta-llama/llama-3.3-70b-instruct` slug in production and beta. OpenRouter remains
the priority-255 concrete fallback in both profiles.

The runtime credential is restricted to the selected Llama model, expires after
30 days, and has a $5 lifetime spending limit. It exists only in the approved secret
boundary and is not committed or printed.

## Exact cost

The reviewed provider mappings are:

| Route | Input / MTok | Output / MTok |
| --- | ---: | ---: |
| DeepInfra direct | $0.10 | $0.32 |
| OpenRouter fallback | $0.13 | $0.40 |

For the reviewed 3,962-prompt / 214-completion token mix, exact Decimal arithmetic
produces `$0.0004646800` through DeepInfra and `$0.0006006600` through OpenRouter.
`usage.cost` reports whichever provider actually served the response; Gateway applies
its fee to that exact amount.

## Compatibility

- The provider model has a 131,072-token context window.
- The route uses OpenAI-compatible Chat Completions at
  `https://api.deepinfra.com/v1/openai`.
- JSON and SSE responses provide prompt and completion usage.
- SSE returns terminal usage and one `[DONE]`.
- The authenticated account reports 200 concurrent requests and 1.1M tokens per
  minute.

## Live activation evidence

Two direct preflight requests proved JSON and SSE compatibility before deployment:
both returned 15 prompt tokens, 2 completion tokens, terminal usage, and exact
`$0.00000214` provider cost.

The initial beta canary completed 10 valid requests: 5 JSON and 5 SSE, with zero
errors. It used 169 prompt and 47 completion tokens, averaged 1,174.260 ms, and
recorded `$0.0000319400` exact DeepInfra cost. Every response selected DeepInfra,
every generation row retained `provider_id = deepinfra`, and each stream included
terminal usage and exactly one `[DONE]`.

Rollback was exercised at the previous immutable GitOps revision. It removed the
DeepInfra route and runtime secret reference while both replicas stayed healthy; beta
continued through OpenRouter. Restoring the reviewed revision returned the next beta
generation to DeepInfra at 382 ms.

Issue #78 later removed the superseded two-price layer and expanded the already
validated DeepInfra route in the checked registry. Production and beta compile the
same DeepInfra-primary/OpenRouter-fallback target order; #78 retains the deployed
production canary and rollback proof.
