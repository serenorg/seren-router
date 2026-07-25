// ABOUTME: Streams authenticated completion requests through the agentgateway sidecar.
// ABOUTME: Preserves upstream status and content type without buffering response bodies.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{StatusCode, header::CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use reqwest::{Client, Url};
use thiserror::Error;

const CHAT_COMPLETIONS_PATH: &str = "v1/chat/completions";
const LEGACY_COMPLETIONS_PATH: &str = "v1/completions";

#[derive(Clone)]
pub struct ProxyState {
    client: Client,
    chat_completions_url: Url,
    legacy_completions_url: Url,
}

impl ProxyState {
    pub fn new(sidecar_url: &str) -> Result<Self, ProxyConfigError> {
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

async fn forward(proxy: ProxyState, endpoint: CompletionEndpoint, body: Bytes) -> Response {
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
    let stream = response.bytes_stream();
    let mut builder = Response::builder().status(status);
    if let Some(content_type) = content_type {
        builder = builder.header(CONTENT_TYPE, content_type);
    }

    builder
        .body(Body::from_stream(stream))
        .expect("status and content-type from reqwest are valid")
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
