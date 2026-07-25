// ABOUTME: Proxies authenticated completion requests through the agentgateway sidecar.
// ABOUTME: Adds exact provider cost to non-streaming JSON while preserving SSE byte streams.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{StatusCode, header::CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::{Number, Value};
use thiserror::Error;

use crate::attribution::ServedProvider;
use crate::pricing::{PriceTable, Usage, cost_usd};

const CHAT_COMPLETIONS_PATH: &str = "v1/chat/completions";
const LEGACY_COMPLETIONS_PATH: &str = "v1/completions";

#[derive(Clone)]
pub struct ProxyState {
    client: Client,
    chat_completions_url: Url,
    legacy_completions_url: Url,
    price_table: Arc<PriceTable>,
}

impl ProxyState {
    pub fn new(sidecar_url: &str, price_table: PriceTable) -> Result<Self, ProxyConfigError> {
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
    let served_provider = ServedProvider::from_headers(response.headers());
    if status.is_success() && served_provider.is_none() {
        tracing::warn!(
            endpoint = url.path(),
            "successful sidecar response omitted served-provider attribution"
        );
    }

    if request.stream || !status.is_success() {
        return downstream_response(
            status,
            content_type,
            Body::from_stream(response.bytes_stream()),
            served_provider,
        );
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
                Ok(body) => Bytes::from(body),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CostOmission {
    InvalidJson,
    MissingUsage,
    InvalidUsage,
    UnknownPrice,
}

impl fmt::Display for CostOmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson => formatter.write_str("response is not valid JSON"),
            Self::MissingUsage => formatter.write_str("response usage object is missing"),
            Self::InvalidUsage => formatter.write_str("response token usage is invalid"),
            Self::UnknownPrice => formatter.write_str("provider/model price is unknown"),
        }
    }
}

fn inject_usage_cost(
    body: &[u8],
    requested_model: &str,
    served_provider: &ServedProvider,
    price_table: &PriceTable,
) -> Result<Vec<u8>, CostOmission> {
    let mut response: Value =
        serde_json::from_slice(body).map_err(|_| CostOmission::InvalidJson)?;
    let usage = response
        .get_mut("usage")
        .and_then(Value::as_object_mut)
        .ok_or(CostOmission::MissingUsage)?;
    let token_usage = Usage {
        prompt_tokens: usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .ok_or(CostOmission::InvalidUsage)?,
        completion_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .ok_or(CostOmission::InvalidUsage)?,
    };
    let canonical_slug = canonical_slug(requested_model, served_provider.as_str());
    let prices = price_table
        .get(served_provider.as_str(), canonical_slug)
        .ok_or(CostOmission::UnknownPrice)?;
    let cost = cost_usd(prices, &token_usage);
    let cost_number =
        Number::from_str(&cost.to_string()).expect("Decimal always serializes as a JSON number");
    usage.insert("cost".to_owned(), Value::Number(cost_number));

    serde_json::to_vec(&response).map_err(|_| CostOmission::InvalidJson)
}

fn canonical_slug<'a>(requested_model: &'a str, served_provider: &str) -> &'a str {
    requested_model
        .strip_prefix(served_provider)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .filter(|slug| !slug.is_empty())
        .unwrap_or(requested_model)
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

    use super::*;
    use crate::registry::{ModelMapping, Provider, Registry};

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
            serde_json::from_slice::<Value>(&transformed).unwrap(),
            expected
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
                inject_usage_cost(candidate, model, &served_provider, &price_table),
                Err(expected),
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
