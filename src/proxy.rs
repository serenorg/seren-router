// ABOUTME: Proxies authenticated completion requests through the agentgateway sidecar.
// ABOUTME: Adds exact provider cost to non-streaming JSON and terminal SSE usage events.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Extension;
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{StatusCode, header::CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use futures::{Stream, StreamExt, stream};
use reqwest::{Client, Url};
use serde_json::Value;
use thiserror::Error;

use crate::attribution::ServedProvider;
use crate::config::RoutingConfig;
use crate::ledger::{GenerationWrite, Ledger};
use crate::policy::measurements::{MeasurementStore, Observation};
use crate::policy::routing::{RouteDecision, RoutingPolicy, RoutingPolicyError};
use crate::pricing::PriceTable;
use crate::registry::Registry;
use crate::routing_profile::RoutingProfile;
use crate::sse::UsageCostTransformer;
use crate::usage_cost::{CostedUsage, inject_usage_cost, prices_for_request};

const CHAT_COMPLETIONS_PATH: &str = "v1/chat/completions";
const LEGACY_COMPLETIONS_PATH: &str = "v1/completions";
const GENERATION_ID_HEADER: &str = "x-generation-id";
#[cfg(feature = "metrics")]
pub(crate) const PROXY_SEGMENT_METRIC: &str = "seren_router_proxy_segment_duration_seconds";

#[derive(Clone)]
pub struct ProxyState {
    client: Client,
    chat_completions_url: Url,
    legacy_completions_url: Url,
    price_table: Arc<PriceTable>,
    ledger: Ledger,
    routing: RoutingPolicy,
}

impl ProxyState {
    pub fn new(
        sidecar_url: &str,
        price_table: PriceTable,
        ledger: Ledger,
        registry: &Registry,
        routing_config: RoutingConfig,
        measurements: MeasurementStore,
    ) -> Result<Self, ProxyConfigError> {
        let normalized = format!("{}/", sidecar_url.trim_end_matches('/'));
        let base_url = Url::parse(&normalized).map_err(|_| ProxyConfigError::InvalidSidecarUrl)?;
        if !matches!(base_url.scheme(), "http" | "https")
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(ProxyConfigError::InvalidSidecarUrl);
        }
        let routing = RoutingPolicy::from_registry(registry, routing_config, measurements)?;

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
            routing,
        })
    }

    pub fn measurements(&self) -> MeasurementStore {
        self.routing.measurements()
    }
}

#[derive(Debug, Error)]
pub enum ProxyConfigError {
    #[error("invalid SEREN_ROUTER_SIDECAR_URL")]
    InvalidSidecarUrl,
    #[error(transparent)]
    InvalidRouting(#[from] RoutingPolicyError),
}

pub async fn chat_completions(
    State(proxy): State<ProxyState>,
    Extension(profile): Extension<RoutingProfile>,
    body: Bytes,
) -> Response {
    forward(proxy, profile, CompletionEndpoint::Chat, body).await
}

pub async fn legacy_completions(
    State(proxy): State<ProxyState>,
    Extension(profile): Extension<RoutingProfile>,
    body: Bytes,
) -> Response {
    forward(proxy, profile, CompletionEndpoint::Legacy, body).await
}

#[derive(Clone, Copy)]
enum CompletionEndpoint {
    Chat,
    Legacy,
}

impl CompletionEndpoint {
    const fn label(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Legacy => "legacy",
        }
    }
}

async fn forward(
    proxy: ProxyState,
    profile: RoutingProfile,
    endpoint: CompletionEndpoint,
    body: Bytes,
) -> Response {
    let request_started_at = Instant::now();
    let mut request = match serde_json::from_slice::<Value>(&body) {
        Ok(request) => request,
        Err(_) => return invalid_json_request(),
    };
    let stream = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or_default();
    let decision = match proxy.routing.route(profile, &mut request) {
        Ok(decision) => decision,
        Err(error) => return error.into_response(),
    };
    let url = match endpoint {
        CompletionEndpoint::Chat => &proxy.chat_completions_url,
        CompletionEndpoint::Legacy => &proxy.legacy_completions_url,
    };
    let selected_body =
        serde_json::to_vec(&request).expect("a parsed JSON completion request serializes");
    let upstream =
        match send_selected_then_fallback(&proxy.client, url, request, selected_body, &decision)
            .await
        {
            Ok(upstream) => upstream,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    endpoint = url.path(),
                    routing_profile = %decision.profile,
                    selected_provider = decision.selected_provider,
                    "sidecar completion request failed"
                );
                return upstream_unavailable();
            }
        };
    let response = upstream.response;

    let status = response.status();
    let content_type = response.headers().get(CONTENT_TYPE).cloned();
    let generation_id = response.headers().get(GENERATION_ID_HEADER).cloned();
    let is_event_stream = content_type
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim() == "text/event-stream");
    let served_provider = ServedProvider::from_headers(response.headers());
    tracing::debug!(
        routing_profile = %decision.profile,
        selected_provider = decision.selected_provider,
        served_provider = served_provider.as_ref().map(ServedProvider::as_str),
        canonical_model = decision.canonical_model,
        "completion route resolved"
    );
    if status.is_success() && served_provider.is_none() {
        tracing::warn!(
            endpoint = url.path(),
            routing_profile = %decision.profile,
            selected_provider = decision.selected_provider,
            "successful sidecar response omitted served-provider attribution"
        );
    }

    if !status.is_success() {
        return downstream_response(
            status,
            content_type,
            generation_id,
            Body::from_stream(response.bytes_stream()),
            served_provider,
        );
    }

    if stream {
        let body = match (is_event_stream, &served_provider) {
            (false, _) => Body::from_stream(response.bytes_stream()),
            (true, Some(served_provider)) => {
                match prices_for_request(
                    &decision.canonical_model,
                    served_provider,
                    &proxy.price_table,
                ) {
                    Ok(prices) => {
                        let generation = GenerationContext::new(
                            proxy.ledger.clone(),
                            proxy.measurements(),
                            &decision.canonical_model,
                            served_provider,
                            status,
                            GenerationTiming::streaming(
                                request_started_at,
                                upstream.attempt_started_at,
                                upstream.headers_received_at,
                                profile,
                            ),
                            endpoint.label(),
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
                            routing_profile = %decision.profile,
                            requested_model = decision.canonical_model,
                            served_provider = served_provider.as_str(),
                            "streaming response cost was omitted"
                        );
                        Body::from_stream(response.bytes_stream())
                    }
                }
            }
            (true, None) => Body::from_stream(response.bytes_stream()),
        };

        return downstream_response(status, content_type, generation_id, body, served_provider);
    }

    let upstream_body = match response.bytes().await {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(
                error = %error,
                endpoint = url.path(),
                routing_profile = %decision.profile,
                selected_provider = decision.selected_provider,
                "failed to read non-streaming sidecar response"
            );
            return upstream_unavailable();
        }
    };
    let response_completed_at = Instant::now();
    let downstream_body = match &served_provider {
        Some(served_provider) => {
            let processing_started_at = Instant::now();
            let costed = inject_usage_cost(
                &upstream_body,
                &decision.canonical_model,
                served_provider,
                &proxy.price_table,
            );
            let processing_duration = processing_started_at.elapsed();
            match costed {
                Ok(costed) => {
                    GenerationContext::new(
                        proxy.ledger.clone(),
                        proxy.measurements(),
                        &decision.canonical_model,
                        served_provider,
                        status,
                        GenerationTiming::non_streaming(
                            request_started_at,
                            upstream.attempt_started_at,
                            upstream.headers_received_at,
                            processing_duration,
                            profile,
                        ),
                        endpoint.label(),
                    )
                    .record_at(costed.usage, response_completed_at);
                    Bytes::from(costed.body)
                }
                Err(reason) => {
                    tracing::warn!(
                        reason = %reason,
                        routing_profile = %decision.profile,
                        requested_model = decision.canonical_model,
                        served_provider = served_provider.as_str(),
                        "non-streaming response cost was omitted"
                    );
                    upstream_body
                }
            }
        }
        None => upstream_body,
    };

    downstream_response(
        status,
        content_type,
        generation_id,
        Body::from(downstream_body),
        served_provider,
    )
}

struct UpstreamResponse {
    response: reqwest::Response,
    attempt_started_at: Instant,
    headers_received_at: Instant,
}

async fn send_selected_then_fallback(
    client: &Client,
    url: &Url,
    mut request: Value,
    selected_body: Vec<u8>,
    decision: &RouteDecision,
) -> Result<UpstreamResponse, reqwest::Error> {
    let selected = send_attempt(client, url, selected_body).await;
    let should_fallback = decision.has_alternatives
        && match &selected {
            Ok(upstream) => retryable_status(upstream.response.status()),
            Err(_) => true,
        };
    if !should_fallback {
        return selected;
    }

    tracing::warn!(
        selected_provider = decision.selected_provider,
        routing_profile = %decision.profile,
        canonical_model = decision.canonical_model,
        "selected provider failed before response commit; retrying through sidecar failover"
    );
    request["model"] = Value::String(decision.fallback_model.clone());
    let fallback_body =
        serde_json::to_vec(&request).expect("a parsed JSON completion request serializes");
    send_attempt(client, url, fallback_body).await
}

async fn send_attempt(
    client: &Client,
    url: &Url,
    body: Vec<u8>,
) -> Result<UpstreamResponse, reqwest::Error> {
    let attempt_started_at = Instant::now();
    let response = client
        .post(url.clone())
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await?;

    Ok(UpstreamResponse {
        response,
        attempt_started_at,
        headers_received_at: Instant::now(),
    })
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn downstream_response(
    status: StatusCode,
    content_type: Option<axum::http::HeaderValue>,
    generation_id: Option<axum::http::HeaderValue>,
    body: Body,
    served_provider: Option<ServedProvider>,
) -> Response {
    let mut builder = Response::builder().status(status);
    if let Some(content_type) = content_type {
        builder = builder.header(CONTENT_TYPE, content_type);
    }
    if let Some(generation_id) = generation_id {
        builder = builder.header(GENERATION_ID_HEADER, generation_id);
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
                        if !chunk.is_empty()
                            && let Some(generation) = generation.as_mut()
                        {
                            generation.observe_first_output(Instant::now());
                        }
                        let current = transformer
                            .take()
                            .expect("transformer is present until the source ends");
                        let processing_started_at = Instant::now();
                        let (next, output, completed) = current.transform(&chunk);
                        if let Some(generation) = generation.as_mut() {
                            generation.observe_processing(processing_started_at.elapsed());
                        }
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
                        let processing_started_at = Instant::now();
                        let (output, completed) = transformer
                            .take()
                            .expect("transformer is present until the source ends")
                            .finish();
                        if let Some(generation) = generation.as_mut() {
                            generation.observe_processing(processing_started_at.elapsed());
                        }
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
    measurements: MeasurementStore,
    canonical_slug: String,
    provider_id: String,
    status: StatusCode,
    request_started_at: Instant,
    attempt_started_at: Instant,
    headers_received_at: Instant,
    first_output_at: Option<Instant>,
    processing_duration: Duration,
    endpoint: &'static str,
    profile: RoutingProfile,
}

struct GenerationTiming {
    request_started_at: Instant,
    attempt_started_at: Instant,
    headers_received_at: Instant,
    first_output_at: Option<Instant>,
    processing_duration: Duration,
    profile: RoutingProfile,
}

impl GenerationTiming {
    fn streaming(
        request_started_at: Instant,
        attempt_started_at: Instant,
        headers_received_at: Instant,
        profile: RoutingProfile,
    ) -> Self {
        Self {
            request_started_at,
            attempt_started_at,
            headers_received_at,
            first_output_at: None,
            processing_duration: Duration::ZERO,
            profile,
        }
    }

    fn non_streaming(
        request_started_at: Instant,
        attempt_started_at: Instant,
        headers_received_at: Instant,
        processing_duration: Duration,
        profile: RoutingProfile,
    ) -> Self {
        Self {
            request_started_at,
            attempt_started_at,
            headers_received_at,
            first_output_at: Some(headers_received_at),
            processing_duration,
            profile,
        }
    }
}

impl GenerationContext {
    fn new(
        ledger: Ledger,
        measurements: MeasurementStore,
        canonical_slug: &str,
        served_provider: &ServedProvider,
        status: StatusCode,
        timing: GenerationTiming,
        endpoint: &'static str,
    ) -> Self {
        Self {
            ledger,
            measurements,
            canonical_slug: canonical_slug.to_owned(),
            provider_id: served_provider.as_str().to_owned(),
            status,
            request_started_at: timing.request_started_at,
            attempt_started_at: timing.attempt_started_at,
            headers_received_at: timing.headers_received_at,
            first_output_at: timing.first_output_at,
            processing_duration: timing.processing_duration,
            endpoint,
            profile: timing.profile,
        }
    }

    fn observe_first_output(&mut self, observed_at: Instant) {
        self.first_output_at.get_or_insert(observed_at);
    }

    fn observe_processing(&mut self, duration: Duration) {
        self.processing_duration = self.processing_duration.saturating_add(duration);
    }

    fn record(self, costed: CostedUsage) {
        self.record_at(costed, Instant::now());
    }

    fn record_at(self, costed: CostedUsage, completed_at: Instant) {
        self.record_measurement(&costed, completed_at);
        self.record_proxy_segments(completed_at);
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
        let latency_ms = i64::try_from(
            completed_at
                .saturating_duration_since(self.request_started_at)
                .as_millis(),
        )
        .unwrap_or(i64::MAX);
        let generation = GenerationWrite {
            id,
            routing_profile: self.profile,
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
                    routing_profile = %generation.routing_profile,
                    provider_id = generation.provider_id,
                    canonical_slug = generation.canonical_slug,
                    "generation ledger insert failed"
                );
            }
        });
    }

    fn record_measurement(&self, costed: &CostedUsage, completed_at: Instant) {
        let Some(first_output_at) = self.first_output_at else {
            return;
        };
        let stream_duration = completed_at.saturating_duration_since(first_output_at);
        if stream_duration.is_zero() {
            tracing::debug!(
                provider_id = self.provider_id,
                canonical_slug = self.canonical_slug,
                "provider measurement omitted because output duration was zero"
            );
            return;
        }
        let observation = Observation {
            completion_tokens: costed.usage.completion_tokens,
            stream_duration,
            time_to_first_token: first_output_at.saturating_duration_since(self.attempt_started_at),
        };
        if let Err(error) = self.measurements.observe_for(
            self.profile,
            &self.provider_id,
            &self.canonical_slug,
            observation,
        ) {
            tracing::debug!(
                error = %error,
                routing_profile = %self.profile,
                provider_id = self.provider_id,
                canonical_slug = self.canonical_slug,
                "provider measurement was omitted"
            );
        }
    }

    fn record_proxy_segments(&self, completed_at: Instant) {
        let Some(first_output_at) = self.first_output_at else {
            return;
        };
        for (segment, duration) in [
            (
                "pre_sidecar",
                self.attempt_started_at
                    .saturating_duration_since(self.request_started_at),
            ),
            (
                "sidecar_headers",
                self.headers_received_at
                    .saturating_duration_since(self.attempt_started_at),
            ),
            (
                "first_output",
                first_output_at.saturating_duration_since(self.attempt_started_at),
            ),
            (
                "sidecar_stream",
                completed_at.saturating_duration_since(self.headers_received_at),
            ),
            (
                "post_first_output",
                completed_at.saturating_duration_since(first_output_at),
            ),
            ("app_processing", self.processing_duration),
            (
                "total",
                completed_at.saturating_duration_since(self.request_started_at),
            ),
        ] {
            record_proxy_segment(
                self.endpoint,
                self.profile,
                &self.provider_id,
                segment,
                duration,
            );
        }
    }

    fn warn_incomplete(&self, reason: &'static str) {
        tracing::warn!(
            reason,
            routing_profile = %self.profile,
            provider_id = self.provider_id,
            canonical_slug = self.canonical_slug,
            "generation ledger record was omitted"
        );
    }
}

#[cfg(feature = "metrics")]
fn record_proxy_segment(
    endpoint: &'static str,
    profile: RoutingProfile,
    provider: &str,
    segment: &'static str,
    duration: Duration,
) {
    metrics::histogram!(
        PROXY_SEGMENT_METRIC,
        "endpoint" => endpoint,
        "profile" => profile.as_str(),
        "provider" => provider.to_owned(),
        "segment" => segment,
    )
    .record(duration.as_secs_f64());
}

#[cfg(not(feature = "metrics"))]
fn record_proxy_segment(
    _endpoint: &'static str,
    _profile: RoutingProfile,
    _provider: &str,
    _segment: &'static str,
    _duration: Duration,
) {
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

fn invalid_json_request() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": {
                "message": "request body must be valid JSON"
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

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
                profiles: BTreeSet::from([RoutingProfile::Production]),
                models: vec![ModelMapping {
                    slug: "functional-model".to_owned(),
                    name: "Functional Model".to_owned(),
                    context_length: 131_072,
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

    #[test]
    fn generation_identifier_is_preserved_for_downstream_metadata_lookups() {
        let response = downstream_response(
            StatusCode::OK,
            None,
            Some(axum::http::HeaderValue::from_static("gen-test")),
            Body::empty(),
            None,
        );

        assert_eq!(response.headers()[GENERATION_ID_HEADER], "gen-test");
    }
}
