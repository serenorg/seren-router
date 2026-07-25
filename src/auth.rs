//! Seren Core passthrough authentication helpers.
//!
//! Use this when a service sits behind Seren Core with `auth_type="passthrough"`
//! and accepts gateway-authenticated users. Production services should verify
//! the SerenCore-signed identity token with [`verify_seren_identity_token`].
//! The raw-header middleware is kept for local development and staged rollout
//! before SerenCore is minting tokens for the service.
//!
//! This stub intentionally keeps the identity model small and generic:
//! - `X-Seren-User-Id` is required
//! - `X-Seren-Organization-Id` is required
//! - `X-Agent-Wallet` is optional and treated as an opaque string
//!
//! Services that need tenant lookup, wallet validation, bounty tokens, or API
//! key validation should extend this module rather than copy `seren-notes` or
//! `seren-swarm` directly.

use std::sync::Arc;

use axum::{
    Json,
    extract::{FromRequestParts, State},
    http::{HeaderMap, Request, StatusCode, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

pub use seren_service_identity::{
    HEADER_SEREN_ORGANIZATION_ID, HEADER_SEREN_USER_ID, IDENTITY_TOKEN_HEADER, JwtError,
    JwtVerifier, VerifiedIdentity,
};
pub const HEADER_AGENT_WALLET: &str = "X-Agent-Wallet";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SerenIdentity {
    pub user_id: Uuid,
    pub organization_id: Uuid,
    pub agent_wallet: Option<String>,
}

impl SerenIdentity {
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, SerenPassthroughAuthError> {
        Ok(Self {
            user_id: required_uuid_header(headers, HEADER_SEREN_USER_ID)?,
            organization_id: required_uuid_header(headers, HEADER_SEREN_ORGANIZATION_ID)?,
            agent_wallet: optional_header(headers, HEADER_AGENT_WALLET),
        })
    }

    /// Build from a verified SerenCore identity token. The agent wallet is not
    /// part of the token, so it is still read from the gateway-injected header.
    pub fn from_verified(verified: &VerifiedIdentity, headers: &HeaderMap) -> Self {
        Self {
            user_id: verified.user_id,
            organization_id: verified.organization_id,
            agent_wallet: optional_header(headers, HEADER_AGENT_WALLET),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SerenPassthroughAuthError(pub String);

impl IntoResponse for SerenPassthroughAuthError {
    fn into_response(self) -> Response {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": self.0,
                    "code": 401
                }
            })),
        )
            .into_response()
    }
}

impl<S> FromRequestParts<S> for SerenIdentity
where
    S: Send + Sync,
{
    type Rejection = SerenPassthroughAuthError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Identity is established by an auth middleware - `verify_seren_identity_token`
        // (fail-closed, production) or `require_seren_passthrough_headers` (header
        // trust, pre-rollout) - both of which insert the `SerenIdentity` extension.
        // The extractor never parses the raw, spoofable headers itself: if the
        // extension is absent, no auth middleware ran for this route, so fail
        // closed rather than silently trusting `X-Seren-*`. This keeps the
        // guarantee local to the extractor instead of depending on every route
        // being layered correctly.
        parts
            .extensions
            .get::<SerenIdentity>()
            .cloned()
            .ok_or_else(|| {
                SerenPassthroughAuthError(
                    "no authenticated identity; wire verify_seren_identity_token or \
                    require_seren_passthrough_headers ahead of this route"
                        .to_string(),
                )
            })
    }
}

pub async fn require_seren_passthrough_headers(
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, SerenPassthroughAuthError> {
    let identity = SerenIdentity::from_headers(request.headers())?;
    request.extensions_mut().insert(identity);
    Ok(next.run(request).await)
}

/// Middleware that verifies a SerenCore-signed identity token (via the shared
/// `seren-service-identity` crate) and, on success, inserts the proven
/// [`SerenIdentity`] into request extensions so the extractor and handlers use
/// cryptographically verified identity instead of the raw, spoofable headers.
///
/// Fail-closed: when this middleware is wired (a verifier is configured) a
/// missing or invalid token is rejected. The audience is this service's
/// publisher slug. The header-only middleware is an explicit local-development
/// fallback and must not be enabled silently when verifier configuration is
/// absent.
///
/// Wire it when a verifier is configured, gate header-only authentication behind
/// an explicit local-development flag, and otherwise fail startup:
///
/// ```ignore
/// use std::sync::Arc;
/// use axum::middleware::{from_fn, from_fn_with_state};
/// use seren_router::auth::{
///     JwtVerifier, require_seren_passthrough_headers, verify_seren_identity_token,
/// };
///
/// let allow_insecure_header_auth = std::env::var("SEREN_ROUTER_ALLOW_INSECURE_HEADER_AUTH")
///     .ok()
///     .is_some_and(|value| value.eq_ignore_ascii_case("true"));
///
/// let app = if let Some(verifier) =
///     JwtVerifier::from_env(&serencore_url, "SEREN_ROUTER_JWT_AUDIENCE")
/// {
///     app.layer(from_fn_with_state(Arc::new(verifier), verify_seren_identity_token))
/// } else if allow_insecure_header_auth {
///     tracing::warn!("using insecure trusted-header authentication");
///     app.layer(from_fn(require_seren_passthrough_headers))
/// } else {
///     anyhow::bail!("identity token verification is not configured");
/// };
/// ```
pub async fn verify_seren_identity_token(
    State(verifier): State<Arc<JwtVerifier>>,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, SerenPassthroughAuthError> {
    // A configured verifier is fail-closed: a missing or invalid token is
    // rejected. `verify_optional` carries the shared missing-token rule; this
    // wrapper maps the verified identity into SerenIdentity. Own the token so
    // the header borrow ends before `extensions_mut`.
    let token = request
        .headers()
        .get(IDENTITY_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let verified = verifier
        .verify_optional(token.as_deref())
        .await
        .map_err(|_| SerenPassthroughAuthError("Invalid identity token".to_string()))?;
    let identity = SerenIdentity::from_verified(&verified, request.headers());
    request.extensions_mut().insert(identity);

    Ok(next.run(request).await)
}

fn required_uuid_header(
    headers: &HeaderMap,
    name: &str,
) -> Result<Uuid, SerenPassthroughAuthError> {
    let value = headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| SerenPassthroughAuthError(format!("Missing {name} header")))?;

    Uuid::parse_str(value).map_err(|_| SerenPassthroughAuthError(format!("Invalid {name} header")))
}

fn optional_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{
        HEADER_AGENT_WALLET, HEADER_SEREN_ORGANIZATION_ID, HEADER_SEREN_USER_ID,
        IDENTITY_TOKEN_HEADER, JwtVerifier, SerenIdentity, require_seren_passthrough_headers,
        verify_seren_identity_token,
    };
    use axum::http::StatusCode;
    use axum::{Json, Router, body::Body, middleware, routing::get};
    use serde_json::json;
    use std::sync::Arc;
    use tower::ServiceExt;
    use uuid::Uuid;

    async fn whoami(identity: SerenIdentity) -> Json<serde_json::Value> {
        Json(json!({
            "user_id": identity.user_id,
            "organization_id": identity.organization_id,
            "agent_wallet": identity.agent_wallet,
        }))
    }

    fn valid_request() -> axum::http::Request<Body> {
        axum::http::Request::builder()
            .uri("/whoami")
            .header(HEADER_SEREN_USER_ID, Uuid::nil().to_string())
            .header(HEADER_SEREN_ORGANIZATION_ID, Uuid::from_u128(1).to_string())
            .header(HEADER_AGENT_WALLET, "0x1234")
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn extractor_without_auth_middleware_fails_closed() {
        // With no auth middleware wired the extension is absent, so the extractor
        // refuses to fall back to the spoofable headers (even valid-looking ones).
        let app = Router::new().route("/whoami", get(whoami));
        let response = app.oneshot(valid_request()).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn middleware_inserts_identity_into_extensions() {
        let app = Router::new()
            .route("/whoami", get(whoami))
            .layer(middleware::from_fn(require_seren_passthrough_headers));

        let response = app.oneshot(valid_request()).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn missing_required_header_returns_401() {
        let app = Router::new()
            .route("/whoami", get(whoami))
            .layer(middleware::from_fn(require_seren_passthrough_headers));

        let request = axum::http::Request::builder()
            .uri("/whoami")
            .header(HEADER_SEREN_USER_ID, Uuid::nil().to_string())
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn missing_agent_wallet_is_allowed() {
        let app = Router::new()
            .route("/whoami", get(whoami))
            .layer(middleware::from_fn(require_seren_passthrough_headers));

        let request = axum::http::Request::builder()
            .uri("/whoami")
            .header(HEADER_SEREN_USER_ID, Uuid::nil().to_string())
            .header(HEADER_SEREN_ORGANIZATION_ID, Uuid::from_u128(1).to_string())
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    // A verifier with no usable keys: enough to exercise the reject branches,
    // since a missing or malformed token resolves before any JWKS fetch. Full
    // token verification is covered in the seren-service-identity crate's tests.
    fn jwt_verifier() -> Arc<JwtVerifier> {
        Arc::new(JwtVerifier::new(
            "http://jwks.invalid/.well-known/jwks.json".to_string(),
            "https://serencore.test".to_string(),
            "seren-router".to_string(),
        ))
    }

    #[tokio::test]
    async fn jwt_rejects_missing_token() {
        let app =
            Router::new()
                .route("/whoami", get(whoami))
                .layer(middleware::from_fn_with_state(
                    jwt_verifier(),
                    verify_seren_identity_token,
                ));

        // Even with valid passthrough headers, a configured verifier requires a
        // valid token (fail-closed).
        let response = app.oneshot(valid_request()).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn jwt_rejects_malformed_token() {
        let app =
            Router::new()
                .route("/whoami", get(whoami))
                .layer(middleware::from_fn_with_state(
                    jwt_verifier(),
                    verify_seren_identity_token,
                ));

        let request = axum::http::Request::builder()
            .uri("/whoami")
            .header(IDENTITY_TOKEN_HEADER, "not-a-jwt")
            .header(HEADER_SEREN_USER_ID, Uuid::nil().to_string())
            .header(HEADER_SEREN_ORGANIZATION_ID, Uuid::from_u128(1).to_string())
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
