<!-- ABOUTME: Compares exact GLM 5.2 provider prices and measured streaming performance. -->
<!-- ABOUTME: Records the bounded benchmark method, results, and resulting route order. -->

# 14 — GLM 5.2 Provider Audit

Audit date: 2026-07-30.

## Outcome

Blackbox was not the cheapest or fastest GLM 5.2 route in this sample.

- OpenRouter had the lowest input, cache-read, and output token prices.
- DeepInfra had the fastest median output rate and the strongest stable
  price/throughput result.
- Blackbox had the highest output price and the lowest median output rate, although
  four repeated prompts received near-complete cache hits.

The checked production and beta default order is therefore DeepInfra, Blackbox, then
OpenRouter. Explicit `provider.sort: price` selects OpenRouter. All three remain
eligible Chat Completions routes. The funded deployment gate passed with exact
provider-cost identity across JSON, SSE, generation metadata, and the raw ledger.

## Reviewed prices

Prices are exact USD per million tokens:

| Route | Input | Cache read | Output |
| --- | ---: | ---: | ---: |
| OpenRouter `z-ai/glm-5.2` | $0.6993 | $0.12987 | $2.1978 |
| DeepInfra `zai-org/GLM-5.2` | $0.75 | $0.14 | $2.40 |
| Blackbox `z-ai/glm-5.2` | $1.40 | $0.14 | $4.40 |

OpenRouter prices came from its live model API. DeepInfra prices came from the
authenticated model catalog: `$0.75/MTok` input, a `0.18666667` cached-input
multiplier resolving to `$0.14/MTok`, and `$2.40/MTok` output. Blackbox prices came
from repeated authenticated paid-response reconciliation.

DeepInfra requires a spend-limited token to name only one model. The deployment
therefore retains the existing `$5` Llama-only token and uses a separate `$5`,
30-day GLM-only token. Both secrets remain confined to AgentGateway.

## Benchmark method

The final comparison used five interleaved, sequential SSE requests per provider:

- identical prompt and `temperature: 0`;
- `max_tokens: 128`;
- terminal usage required;
- exactly one `[DONE]` required;
- TTFT measured from request start to the first actual content or reasoning delta;
- output rate calculated as completion tokens divided by elapsed time from the first
  semantic token through `[DONE]`; and
- exact request cost recomputed from prompt, cached-prompt, and completion telemetry
  using the reviewed route prices.

OpenRouter selected CoreWeave, Fireworks, and GMICloud during the sample. Direct
DeepInfra and Blackbox requests did not expose a nested upstream name.

An earlier incomplete preflight observed a cold DeepInfra request with 34.649-second
TTFT and 24.37 output tokens/second. It is excluded from the balanced five-request
table because the same round's OpenRouter response used an initially unrecognized
reasoning delta field, but the cold observation remains part of the operational
record.

## Results

| Route | Median TTFT | Median output tok/s | Mean output tok/s | Range tok/s | Exact five-request cost |
| --- | ---: | ---: | ---: | ---: | ---: |
| OpenRouter | 0.912 s | 97.075 | 134.942 | 65.840–227.334 | $0.0015499485 |
| DeepInfra | 1.084 s | 132.648 | 133.218 | 105.165–171.692 | $0.0016897500 |
| Blackbox | 1.393 s | 55.678 | 64.786 | 50.800–98.577 | $0.0028510000 |

Each route produced 640 completion tokens and 205 prompt tokens. Blackbox reported
200 cached prompt tokens; DeepInfra and OpenRouter reported none. Exact cost uses
those observed cache counts rather than assuming uniform cache behavior.

For a compact stable-value comparison, median output tok/s divided by output
USD/MTok was:

| Route | Median tok/s per output USD/MTok |
| --- | ---: |
| DeepInfra | 55.270 |
| OpenRouter | 44.169 |
| Blackbox | 12.654 |

This ratio is a routing comparison, not a billing unit. Token price and throughput
remain separate measurements: OpenRouter is cheapest per token; DeepInfra delivered
the strongest median speed per published output price in this bounded run.

## Deployed gate

The reviewed source `1ab4671383d247ec83fd86daf8982f9d31f3deee` was deployed as
Linux ARM64 image
`sha256:e1d7023d11f0c545501c70c1e0d48829c957de13798bb5cf85d745160dea4b36`
through GitOps revision `185bc658f69d35e208e5432a73d49a40557ecc1e`.
Only the router Deployment, registry ConfigMap, ExternalSecret, PDB, and keep-warm
CronJob were selectively synced.

The final bounded gate made six routed requests:

- production and beta JSON selected `deepinfra-glm`;
- production and beta SSE selected `deepinfra-glm`, each returning exactly one
  terminal usage event and one `[DONE]`;
- every DeepInfra GLM response reported 17 prompt tokens, 8 completion tokens, and
  exact provider cost `$0.0000319500`;
- no public response leaked `estimated_cost`;
- production `provider.sort: price` selected OpenRouter and recorded exact provider
  cost `$0.0000322800`; and
- production Llama selected DeepInfra and recorded exact provider cost
  `$0.0000021400`.

All six generation metadata lookups matched the response cost. The six raw ledger
rows recorded the expected provider and routing profile with
`cost_usd = provider_cost_usd`, `sell_price_usd IS NULL`, and status 200. The full
same-request GLM fallback chain remains DeepInfra, Blackbox, then OpenRouter. Two
earlier selective rollback exercises restored the preceding OpenRouter-safe image
and catalog before the final corrected restore.

Final runtime evidence was 2/2 Ready across `us-east-1a` and `us-east-1c`, zero
container restarts, healthy PDB and EndpointSlices, public health 200/200, database
availability metric 1, successful keep-warm execution, a Ready ExternalSecret, zero
serious router or AgentGateway log lines, and no router Warning events.
