// ABOUTME: Proxies authenticated completion requests through the agentgateway sidecar.
// ABOUTME: Adds exact provider cost to non-streaming JSON and terminal SSE usage events.

use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{StatusCode, header::CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use futures::{Stream, StreamExt, stream};
use reqwest::{Client, Url};
use serde::Deserialize;
use thiserror::Error;

use crate::attribution::ServedProvider;
use crate::ledger::{GenerationWrite, Ledger};
use crate::pricing::PriceTable;
use crate::sse::UsageCostTransformer;
use crate::usage_cost::{CostedUsage, canonical_slug, inject_usage_cost, prices_for_request};

const CHAT_COMPLETIONS_PATH: &str = "v1/chat/completions";
const LEGACY_COMPLETIONS_PATH: &str = "v1/completions";

#[derive(Clone)]
pub struct ProxyState {
    client: Client,
    chat_completions_url: Url,
    legacy_completions_url: Url,
    price_table: Arc<PriceTable>,
    ledger: Ledger,
}

impl ProxyState {
    pub fn new(
        sidecar_url: &str,
        price_table: PriceTable,
        ledger: Ledger,
    ) -> Result<Self, ProxyConfigError> {
        let normalized = format!("{}/", sidecar_url.trim_end_matches('/'));
        let base_url = Url::parse(&normalized).map_err(|_| ProxyConfigError)?;
        if !matches!(base_url.scheme(), "http" | "https")
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(ProxyConfigError);
        }

        Ok(Self {
            client: Client::new(),
            chat_completions_url: base_url
                .join(CHAT_COMPLETIONS_PATH)
                .expect("constant chat-completions path is valid"),
            legacy_completions_url: base_url
                .join(LEGACY_COMPLETIONS_PATH)
                .expect("constant legacy-completions path is valid"),
            price_table: Arc::new(price_table),
            ledger,
        })
    }
}

#[derive(Debug, Error)]
#[error("invalid SEREN_ROUTER_SIDECAR_URL")]
pub struct ProxyConfigError;

pub async fn chat_completions(State(proxy): State<ProxyState>, body: Bytes) -> Response {
    forward(proxy, CompletionEndpoint::Chat, body).await
}

pub async fn legacy_completions(State(proxy): State<ProxyState>, body: Bytes) -> Response {
    forward(proxy, CompletionEndpoint::Legacy, body).await
}

#[derive(Clone, Copy)]
enum CompletionEndpoint {
    Chat,
    Legacy,
}

#[derive(Default, Deserialize)]
struct CompletionRequestMetadata {
    model: Option<String>,
    #[serde(default)]
    stream: bool,
}

async fn forward(proxy: ProxyState, endpoint: CompletionEndpoint, body: Bytes) -> Response {
    let started_at = Instant::now();
    let request = serde_json::from_slice::<CompletionRequestMetadata>(&body).unwrap_or_default();
    let url = match endpoint {
        CompletionEndpoint::Chat => &proxy.chat_completions_url,
        CompletionEndpoint::Legacy => &proxy.legacy_completions_url,
    };
    let response = match proxy
        .client
        .post(url.clone())
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(
                error = %error,
                endpoint = url.path(),
                "sidecar completion request failed"
            );
            return upstream_unavailable();
        }
    };

    let status = response.status();
    let content_type = response.headers().get(CONTENT_TYPE).cloned();
    let is_event_stream = content_type
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim() == "text/event-stream");
    let served_provider = ServedProvider::from_headers(response.headers());
    if status.is_success() && served_provider.is_none() {
        tracing::warn!(
            endpoint = url.path(),
            "successful sidecar response omitted served-provider attribution"
        );
    }

    if !status.is_success() {
        return downstream_response(
            status,
            content_type,
            Body::from_stream(response.bytes_stream()),
            served_provider,
        );
    }

    if request.stream {
        let body = match (is_event_stream, &request.model, &served_provider) {
            (false, _, _) => Body::from_stream(response.bytes_stream()),
            (true, Some(requested_model), Some(served_provider)) => {
                match prices_for_request(requested_model, served_provider, &proxy.price_table) {
                    Ok(prices) => {
                        let generation = GenerationContext::new(
                            proxy.ledger.clone(),
                            requested_model,
                            served_provider,
                            status,
                            started_at,
                        );
                        Body::from_stream(stream_with_usage_cost(
                            response.bytes_stream(),
                            UsageCostTransformer::new(prices.clone()),
                            generation,
                        ))
                    }
                    Err(reason) => {
                        tracing::warn!(
                            reason = %reason,
                            requested_model,
                            served_provider = served_provider.as_str(),
                            "streaming response cost was omitted"
                        );
                        Body::from_stream(response.bytes_stream())
                    }
                }
            }
            (true, None, _) => {
                tracing::warn!("streaming response cost was omitted: request model is missing");
                Body::from_stream(response.bytes_stream())
            }
            (true, _, None) => Body::from_stream(response.bytes_stream()),
        };

        return downstream_response(status, content_type, body, served_provider);
    }

    let upstream_body = match response.bytes().await {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(
                error = %error,
                endpoint = url.path(),
                "failed to read non-streaming sidecar response"
            );
            return upstream_unavailable();
        }
    };
    let downstream_body = match (&request.model, &served_provider) {
        (Some(requested_model), Some(served_provider)) => {
            match inject_usage_cost(
                &upstream_body,
                requested_model,
                served_provider,
                &proxy.price_table,
            ) {
                Ok(costed) => {
                    GenerationContext::new(
                        proxy.ledger.clone(),
                        requested_model,
                        served_provider,
                        status,
                        started_at,
                    )
                    .record(costed.usage);
                    Bytes::from(costed.body)
                }
                Err(reason) => {
                    tracing::warn!(
                        reason = %reason,
                        requested_model,
                        served_provider = served_provider.as_str(),
                        "non-streaming response cost was omitted"
                    );
                    upstream_body
                }
            }
        }
        (None, _) => {
            tracing::warn!("non-streaming response cost was omitted: request model is missing");
            upstream_body
        }
        (_, None) => upstream_body,
    };

    downstream_response(
        status,
        content_type,
        Body::from(downstream_body),
        served_provider,
    )
}

fn downstream_response(
    status: StatusCode,
    content_type: Option<axum::http::HeaderValue>,
    body: Body,
    served_provider: Option<ServedProvider>,
) -> Response {
    let mut builder = Response::builder().status(status);
    if let Some(content_type) = content_type {
        builder = builder.header(CONTENT_TYPE, content_type);
    }

    let mut downstream = builder
        .body(body)
        .expect("status and content-type from reqwest are valid");
    if let Some(served_provider) = served_provider {
        downstream.extensions_mut().insert(served_provider);
    }

    downstream
}

fn stream_with_usage_cost<S>(
    input: S,
    transformer: UsageCostTransformer,
    generation: GenerationContext,
) -> impl Stream<Item = Result<Bytes, reqwest::Error>>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    stream::unfold(
        (Box::pin(input), Some(transformer), Some(generation)),
        |(mut input, mut transformer, mut generation)| async move {
            transformer.as_ref()?;
            loop {
                match input.next().await {
                    Some(Ok(chunk)) => {
                        let current = transformer
                            .take()
                            .expect("transformer is present until the source ends");
                        let (next, output, completed) = current.transform(&chunk);
                        transformer = Some(next);
                        if let Some(completed) = completed
                            && let Some(generation) = generation.take()
                        {
                            generation.record(completed);
                        }
                        if !output.is_empty() {
                            return Some((
                                Ok(Bytes::from(output)),
                                (input, transformer, generation),
                            ));
                        }
                    }
                    Some(Err(error)) => {
                        if let Some(generation) = generation.take() {
                            generation.warn_incomplete("upstream stream returned an error");
                        }
                        return Some((Err(error), (input, None, generation)));
                    }
                    None => {
                        let (output, completed) = transformer
                            .take()
                            .expect("transformer is present until the source ends")
                            .finish();
                        if let Some(completed) = completed
                            && let Some(generation) = generation.take()
                        {
                            generation.record(completed);
                        }
                        if let Some(generation) = generation.take() {
                            generation.warn_incomplete(
                                "stream ended before a costed usage event and [DONE]",
                            );
                        }
                        return (!output.is_empty())
                            .then(|| (Ok(Bytes::from(output)), (input, None, generation)));
                    }
                }
            }
        },
    )
}

struct GenerationContext {
    ledger: Ledger,
    canonical_slug: String,
    provider_id: String,
    status: StatusCode,
    started_at: Instant,
}

impl GenerationContext {
    fn new(
        ledger: Ledger,
        requested_model: &str,
        served_provider: &ServedProvider,
        status: StatusCode,
        started_at: Instant,
    ) -> Self {
        Self {
            ledger,
            canonical_slug: canonical_slug(requested_model, served_provider.as_str()).to_owned(),
            provider_id: served_provider.as_str().to_owned(),
            status,
            started_at,
        }
    }

    fn record(self, costed: CostedUsage) {
        let Some(id) = costed.response_id else {
            self.warn_incomplete("costed response omitted a provider response id");
            return;
        };
        let Ok(prompt_tokens) = i64::try_from(costed.usage.prompt_tokens) else {
            self.warn_incomplete("prompt token count exceeds the ledger BIGINT");
            return;
        };
        let Ok(completion_tokens) = i64::try_from(costed.usage.completion_tokens) else {
            self.warn_incomplete("completion token count exceeds the ledger BIGINT");
            return;
        };
        let latency_ms = i64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(i64::MAX);
        let generation = GenerationWrite {
            id,
            canonical_slug: self.canonical_slug,
            provider_id: self.provider_id,
            prompt_tokens,
            completion_tokens,
            cost_usd: costed.cost_usd,
            latency_ms,
            status: i16::try_from(self.status.as_u16()).expect("HTTP status fits in SMALLINT"),
        };
        let ledger = self.ledger;

        tokio::spawn(async move {
            if let Err(error) = ledger.insert(&generation).await {
                tracing::error!(
                    error = %error,
                    generation_id = generation.id,
                    provider_id = generation.provider_id,
                    canonical_slug = generation.canonical_slug,
                    "generation ledger insert failed"
                );
            }
        });
    }

    fn warn_incomplete(&self, reason: &'static str) {
        tracing::warn!(
            reason,
            provider_id = self.provider_id,
            canonical_slug = self.canonical_slug,
            "generation ledger record was omitted"
        );
    }
}

fn upstream_unavailable() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(serde_json::json!({
            "error": {
                "message": "upstream unavailable"
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;
    use serde_json::Value;

    use super::*;
    use crate::registry::{ModelMapping, Provider, Registry};
    use crate::usage_cost::CostOmission;

    #[test]
    fn real_non_streaming_response_gains_exact_usage_cost() {
        let body = include_bytes!("../tests/fixtures/nonstreaming_chat_response.json");
        let expected: Value = serde_json::from_slice(include_bytes!(
            "../tests/golden/nonstreaming_chat_cost.json"
        ))
        .unwrap();
        let price_table = test_price_table();
        let served_provider = served_provider("local");

        let transformed = inject_usage_cost(
            body,
            "local/functional-model",
            &served_provider,
            &price_table,
        )
        .unwrap();

        assert_eq!(
            serde_json::from_slice::<Value>(&transformed.body).unwrap(),
            expected
        );
        assert_eq!(
            transformed.usage,
            CostedUsage {
                response_id: Some("chatcmpl-lk0p4fm7w70wido9snoqp".to_owned()),
                usage: crate::pricing::Usage {
                    prompt_tokens: 16,
                    completion_tokens: 3,
                },
                cost_usd: "0.0000088000".parse().unwrap(),
            }
        );
        assert!(expected.get("provider").is_none());
    }

    #[test]
    fn bookkeeping_misses_are_reported_without_a_replacement_body() {
        let body = br#"{"id":"chatcmpl-1","usage":{"prompt_tokens":7,"completion_tokens":3}}"#;
        let price_table = test_price_table();
        let served_provider = served_provider("local");

        for (name, candidate, model, expected) in [
            (
                "missing usage",
                br#"{"id":"chatcmpl-1"}"#.as_slice(),
                "functional-model",
                CostOmission::MissingUsage,
            ),
            (
                "unknown price",
                body.as_slice(),
                "unknown-model",
                CostOmission::UnknownPrice,
            ),
        ] {
            assert_eq!(
                inject_usage_cost(candidate, model, &served_provider, &price_table).err(),
                Some(expected),
                "{name}"
            );
        }
    }

    fn test_price_table() -> PriceTable {
        PriceTable::from_registry(&Registry {
            providers: vec![Provider {
                id: "local".to_owned(),
                display_name: "Local".to_owned(),
                base_url: "http://127.0.0.1:1234/v1".to_owned(),
                secret_env: "TEST_ONLY".to_owned(),
                enabled: true,
                priority: 0,
                models: vec![ModelMapping {
                    slug: "functional-model".to_owned(),
                    provider_model_id: "local-model".to_owned(),
                    input_price_per_mtok: Decimal::new(40, 2),
                    output_price_per_mtok: Decimal::new(80, 2),
                }],
            }],
        })
        .unwrap()
    }

    fn served_provider(provider: &str) -> ServedProvider {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            crate::attribution::SERVED_PROVIDER_HEADER,
            provider.parse().unwrap(),
        );
        ServedProvider::from_headers(&headers).unwrap()
    }
}
