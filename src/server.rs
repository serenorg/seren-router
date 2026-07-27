//! Production-ready server infrastructure for Seren microservices.
//!
//! Provides Kubernetes health probes, structured logging, request correlation IDs,
//! graceful shutdown with SIGTERM support, panic recovery, and feature-gated
//! production middleware (metrics, security headers, compression, rate limiting).
//!
//! # Feature Flags
//!
//! Enable in `Cargo.toml`:
//! - `metrics` - Prometheus `/metrics` endpoint
//! - `security-headers` - X-Frame-Options, X-Content-Type-Options
//! - `compression` - gzip/brotli/zstd response compression
//! - `payload-limit` - reject oversized request bodies
//! - `sensitive-headers` - redact Authorization from logs
//! - `cors` - permissive CORS (for browser-facing services)
//! - `rate-limiting` - per-IP request throttling
//! - `concurrency-limit` - max in-flight requests
//! - `production` - metrics + security-headers + payload-limit + sensitive-headers
//! - `full` - all of the above + cors + compression + rate-limiting + concurrency-limit

use axum::{
    Extension, Router,
    http::{HeaderName, Request, StatusCode},
    response::IntoResponse,
    routing::get,
};
use reqwest::{Client, Url, redirect::Policy};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tower_http::{
    catch_panic::CatchPanicLayer,
    normalize_path::NormalizePathLayer,
    request_id::{MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::{ContextV7, Timestamp, Uuid};

use crate::db::DatabaseHealth;

/// Default timeout for metadata, health, and other short-lived requests.
const STANDARD_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Inference timeout. LLM streams routinely remain open for several minutes.
const INFERENCE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Graceful shutdown timeout.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// A readiness dependency must fail quickly enough for Kubernetes to reroute traffic.
const READINESS_DEPENDENCY_TIMEOUT: Duration = Duration::from_secs(2);

/// Prometheus metrics endpoint path.
#[cfg(feature = "metrics")]
const METRICS_ROUTE: &str = "/metrics";

/// Maximum request body size for ordinary service routes (1 MiB).
#[cfg(feature = "payload-limit")]
const STANDARD_PAYLOAD_SIZE_BYTES: usize = 1_048_576;

/// Maximum request body size for inference routes carrying vision inputs (20 MiB).
#[cfg(feature = "payload-limit")]
const INFERENCE_PAYLOAD_SIZE_BYTES: usize = 20 * 1_048_576;

/// Maximum requests per second per IP.
#[cfg(feature = "rate-limiting")]
const MAX_REQUESTS_PER_SEC: u32 = 100;

/// Maximum concurrent in-flight requests.
#[cfg(feature = "concurrency-limit")]
const MAX_CONCURRENT_REQUESTS: usize = 1024;

// ---------------------------------------------------------------------------
// Tracing
// ---------------------------------------------------------------------------

/// Initialize structured logging.
///
/// - In Kubernetes (detected via `KUBERNETES_SERVICE_HOST`): JSON format.
/// - Locally: human-readable plain text.
/// - Respects `RUST_LOG` env var for filtering.
pub fn setup_tracing(default_filter: &str) {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| default_filter.into());

    let in_k8s = std::env::var("KUBERNETES_SERVICE_HOST").is_ok()
        || std::env::var("KUBERNETES_PORT").is_ok();

    if in_k8s {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
    }
}

// ---------------------------------------------------------------------------
// Request ID (UUIDv7)
// ---------------------------------------------------------------------------

/// Generates UUIDv7 request IDs for correlation across services.
///
/// Preserves an existing `x-request-id` header if present (e.g. from a gateway),
/// otherwise generates a new UUIDv7 which embeds a millisecond timestamp for
/// natural chronological ordering.
#[derive(Clone)]
struct RequestIdGenerator;

impl MakeRequestId for RequestIdGenerator {
    fn make_request_id<B>(&mut self, request: &Request<B>) -> Option<RequestId> {
        if let Some(existing) = request.headers().get("x-request-id") {
            return Some(RequestId::new(existing.clone()));
        }
        let cx = ContextV7::new();
        let uuid = Uuid::new_v7(Timestamp::now(cx));
        let value = uuid.to_string().parse().ok()?;
        Some(RequestId::new(value))
    }
}

// ---------------------------------------------------------------------------
// Shared state for health probes
// ---------------------------------------------------------------------------

struct ServerState {
    database_health: DatabaseHealth,
    sidecar_client: Client,
    sidecar_readiness_url: Url,
}

// ---------------------------------------------------------------------------
// Health probes
// ---------------------------------------------------------------------------

/// Liveness probe - is the process alive and not deadlocked?
///
/// This must be cheap and dependency-free. If this endpoint stops responding,
/// Kubernetes restarts the pod. Never add database or network checks here.
async fn livez() -> impl IntoResponse {
    StatusCode::OK
}

/// Readiness probe - is the service ready to accept traffic?
///
/// Gates on AgentGateway, the synchronous inference dependency. PostgreSQL is
/// an asynchronous ledger dependency and is reported without taking inference
/// out of service.
async fn readyz(Extension(state): Extension<Arc<ServerState>>) -> impl IntoResponse {
    state.database_health.record_availability();
    let database_status = state.database_health.status().as_str();
    match state
        .sidecar_client
        .get(state.sidecar_readiness_url.clone())
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => (
            StatusCode::OK,
            axum::Json(serde_json::json!({
                "status": "ok",
                "dependencies": {
                    "database": database_status,
                    "sidecar": "ok"
                }
            })),
        ),
        Ok(response) => {
            tracing::warn!(
                status = %response.status(),
                "readyz: AgentGateway check returned an unhealthy status"
            );
            unavailable(database_status)
        }
        Err(error) => {
            tracing::warn!(error = %error, "readyz: AgentGateway check failed");
            unavailable(database_status)
        }
    }
}

fn unavailable(database_status: &'static str) -> (StatusCode, axum::Json<serde_json::Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(serde_json::json!({
            "status": "unavailable",
            "reason": "sidecar",
            "dependencies": {
                "database": database_status,
                "sidecar": "unavailable"
            }
        })),
    )
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

fn handle_panic(err: Box<dyn std::any::Any + Send + 'static>) -> axum::response::Response {
    let msg = if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = err.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "unknown panic".to_string()
    };

    tracing::error!(panic = %msg, "service panicked");

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(serde_json::json!({ "error": "internal server error" })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Graceful shutdown
// ---------------------------------------------------------------------------

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install CTRL+C handler");
        tracing::info!("received CTRL+C");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
        tracing::info!("received SIGTERM");
    };

    #[cfg(unix)]
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }

    #[cfg(not(unix))]
    ctrl_c.await;

    tracing::info!("shutdown signal received, draining connections");

    // Force-exit if axum's graceful drain does not finish within
    // SHUTDOWN_TIMEOUT. Detached so the drain proceeds in parallel;
    // whichever finishes first wins.
    tokio::spawn(async {
        tokio::time::sleep(SHUTDOWN_TIMEOUT).await;
        tracing::warn!(
            timeout_secs = SHUTDOWN_TIMEOUT.as_secs(),
            "graceful shutdown timed out, forcing exit"
        );
        std::process::exit(1);
    });
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Apply the standard timeout and payload cap to short-lived service routes.
pub fn standard_route_policies(router: Router) -> Router {
    route_policies(router, STANDARD_REQUEST_TIMEOUT, standard_payload_size())
}

/// Apply limits sized for chat/completion streams and base64 vision requests.
pub fn inference_route_policies(router: Router) -> Router {
    route_policies(router, INFERENCE_REQUEST_TIMEOUT, inference_payload_size())
}

fn route_policies(router: Router, timeout: Duration, payload_size: Option<usize>) -> Router {
    let router = router.layer(TimeoutLayer::with_status_code(
        StatusCode::REQUEST_TIMEOUT,
        timeout,
    ));

    #[cfg(feature = "payload-limit")]
    let router = {
        use axum::extract::DefaultBodyLimit;
        use tower_http::limit::RequestBodyLimitLayer;

        router
            .layer(DefaultBodyLimit::disable())
            .layer(RequestBodyLimitLayer::new(
                payload_size.expect("payload size is configured with payload-limit"),
            ))
    };

    #[cfg(not(feature = "payload-limit"))]
    let _ = payload_size;

    router
}

#[cfg(feature = "payload-limit")]
const fn standard_payload_size() -> Option<usize> {
    Some(STANDARD_PAYLOAD_SIZE_BYTES)
}

#[cfg(not(feature = "payload-limit"))]
const fn standard_payload_size() -> Option<usize> {
    None
}

#[cfg(feature = "payload-limit")]
const fn inference_payload_size() -> Option<usize> {
    Some(INFERENCE_PAYLOAD_SIZE_BYTES)
}

#[cfg(not(feature = "payload-limit"))]
const fn inference_payload_size() -> Option<usize> {
    None
}

/// Start an Axum server with production middleware already wired in.
///
/// ## Middleware stack (outermost -> innermost)
///
/// | # | Middleware | Feature |
/// |---|-----------|---------|
/// | 1 | Panic recovery | always |
/// | 2 | Request ID (UUIDv7) | always |
/// | 3 | Rate limiting | `rate-limiting` |
/// | 4 | Metrics (Prometheus) | `metrics` |
/// | 5 | Logging (TraceLayer) | always |
/// | 6 | Security headers (Helmet) | `security-headers` |
/// | 7 | CORS | `cors` |
/// | 8 | Sensitive headers | `sensitive-headers` |
/// | 9 | Path normalization | always |
/// | 10 | Compression | `compression` |
/// | 11 | Concurrency limit | `concurrency-limit` |
/// | 12 | Liveness + readiness probes | always |
///
/// Timeout and payload policies are route-class specific. Route registries must
/// apply [`standard_route_policies`] or [`inference_route_policies`] before the
/// router reaches this shared stack.
///
/// AgentGateway must be reachable for `/readyz` to return 200. PostgreSQL state
/// remains observable but does not gate inference readiness.
pub async fn serve(
    app: Router,
    database_health: DatabaseHealth,
    sidecar_readiness_url: Url,
) -> anyhow::Result<()> {
    let x_request_id = HeaderName::from_static("x-request-id");

    // --- innermost layers first (health probes, then feature-gated, then always-on) ---

    let health = health_router(database_health.clone(), sidecar_readiness_url)?;
    let app = app.merge(health);

    // Concurrency limit
    #[cfg(feature = "concurrency-limit")]
    let app = {
        use tower::limit::ConcurrencyLimitLayer;
        tracing::info!(MAX_CONCURRENT_REQUESTS, "concurrency limit enabled");
        app.layer(ConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS))
    };

    // Response compression + request decompression
    #[cfg(feature = "compression")]
    let app = {
        use tower_http::{compression::CompressionLayer, decompression::RequestDecompressionLayer};
        tracing::info!("compression enabled");
        app.layer(RequestDecompressionLayer::new())
            .layer(CompressionLayer::new())
    };

    // Path normalization (always-on)
    let app = app.layer(NormalizePathLayer::trim_trailing_slash());

    // Sensitive header redaction
    #[cfg(feature = "sensitive-headers")]
    let app = {
        use tower_http::sensitive_headers::SetSensitiveHeadersLayer;
        tracing::info!("sensitive header redaction enabled");
        app.layer(SetSensitiveHeadersLayer::new(std::iter::once(
            axum::http::header::AUTHORIZATION,
        )))
    };

    // CORS
    #[cfg(feature = "cors")]
    let app = {
        use tower_http::cors::CorsLayer;
        tracing::info!("CORS enabled");
        app.layer(CorsLayer::permissive())
    };

    // Security headers (Helmet)
    #[cfg(feature = "security-headers")]
    let app = {
        use axum_helmet::Helmet;
        tracing::info!("security headers enabled");
        let helmet = Helmet::new()
            .add(helmet_core::XContentTypeOptions::nosniff())
            .add(helmet_core::XFrameOptions::deny());
        app.layer(helmet.into_layer()?)
    };

    // Logging (always-on)
    let app = app.layer(TraceLayer::new_for_http().make_span_with(
        |request: &Request<axum::body::Body>| {
            let request_id = request
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown");

            tracing::info_span!(
                "http_request",
                method = %request.method(),
                uri = %request.uri(),
                request_id = %request_id,
            )
        },
    ));

    // Prometheus metrics
    #[cfg(feature = "metrics")]
    let app = {
        let (prometheus_layer, metrics_handle) =
            axum_prometheus::PrometheusMetricLayerBuilder::new()
                .with_prefix(env!("CARGO_PKG_NAME"))
                .with_ignore_pattern(METRICS_ROUTE)
                .with_default_metrics()
                .build_pair();
        metrics::describe_histogram!(
            crate::proxy::PROXY_SEGMENT_METRIC,
            metrics::Unit::Seconds,
            "Duration of successful costed completion request lifecycle segments."
        );
        crate::db::describe_metrics();
        database_health.record_availability();
        tracing::info!(path = METRICS_ROUTE, "prometheus metrics enabled");
        app.route(
            METRICS_ROUTE,
            get(|| async move { metrics_handle.render() }),
        )
        .layer(prometheus_layer)
    };

    // Per-IP rate limiting
    #[cfg(feature = "rate-limiting")]
    let app = {
        use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};
        tracing::info!(MAX_REQUESTS_PER_SEC, "rate limiting enabled");
        let governor_conf = GovernorConfigBuilder::default()
            .per_nanosecond((1_000_000_000 / MAX_REQUESTS_PER_SEC) as u64)
            .burst_size(MAX_REQUESTS_PER_SEC)
            .finish()
            .expect("failed to build rate limiter config");
        app.layer(GovernorLayer::new(Box::new(governor_conf)))
    };

    // Request ID (always-on)
    let app = app
        .layer(PropagateRequestIdLayer::new(x_request_id.clone()))
        .layer(SetRequestIdLayer::new(x_request_id, RequestIdGenerator));

    // Panic recovery (always-on, outermost)
    let app = app.layer(CatchPanicLayer::custom(handle_panic));

    // --- start server ---

    let bind = std::env::var("BIND_ADDRESS").unwrap_or_else(|_| "0.0.0.0:8000".to_string());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(address = %bind, "listening");

    let service = app.into_make_service_with_connect_info::<SocketAddr>();

    axum::serve(listener, service)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("server stopped");

    Ok(())
}

/// Build dependency-aware health routes for the production server and real functional gates.
pub fn health_router(
    database_health: DatabaseHealth,
    sidecar_readiness_url: Url,
) -> anyhow::Result<Router> {
    let sidecar_client = Client::builder()
        .connect_timeout(READINESS_DEPENDENCY_TIMEOUT)
        .timeout(READINESS_DEPENDENCY_TIMEOUT)
        .redirect(Policy::none())
        .no_proxy()
        .build()?;
    let state = Arc::new(ServerState {
        database_health,
        sidecar_client,
        sidecar_readiness_url,
    });
    Ok(standard_route_policies(
        Router::new()
            .route("/livez", get(livez))
            .route("/readyz", get(readyz)),
    )
    .layer(Extension(state)))
}

#[cfg(all(test, feature = "payload-limit"))]
mod tests {
    use axum::{
        Router,
        body::{Body, Bytes},
        http::{Request, StatusCode},
        routing::post,
    };
    use tower::ServiceExt;

    use super::{STANDARD_PAYLOAD_SIZE_BYTES, inference_route_policies, standard_route_policies};

    fn body_reader() -> Router {
        Router::new().route("/", post(|_: Bytes| async { StatusCode::OK }))
    }

    #[tokio::test]
    async fn inference_policy_accepts_vision_body_rejected_by_standard_policy() {
        let body = vec![0; STANDARD_PAYLOAD_SIZE_BYTES + 1];

        let standard = standard_route_policies(body_reader())
            .oneshot(Request::post("/").body(Body::from(body.clone())).unwrap())
            .await
            .unwrap();
        let inference = inference_route_policies(body_reader())
            .oneshot(Request::post("/").body(Body::from(body)).unwrap())
            .await
            .unwrap();

        assert_eq!(standard.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(inference.status(), StatusCode::OK);
    }
}
