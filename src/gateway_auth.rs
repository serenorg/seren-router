// ABOUTME: Authenticates the Seren Gateway's static bearer credential.
// ABOUTME: Compares key bytes in constant time and never logs presented credentials.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header::AUTHORIZATION};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use subtle::ConstantTimeEq;

use crate::routing_profile::RoutingProfile;

#[derive(Clone)]
pub struct GatewayAuth {
    production_key: Arc<[u8]>,
    beta_key: Option<Arc<[u8]>>,
}

impl GatewayAuth {
    pub fn new(expected_key: impl AsRef<[u8]>) -> Self {
        Self {
            production_key: Arc::from(expected_key.as_ref()),
            beta_key: None,
        }
    }

    pub fn with_beta_key(mut self, beta_key: impl AsRef<[u8]>) -> Self {
        self.beta_key = Some(Arc::from(beta_key.as_ref()));
        self
    }

    fn profile_for(&self, provided: &[u8]) -> Option<RoutingProfile> {
        let production_match = bool::from(self.production_key.as_ref().ct_eq(provided));
        let beta_match = self
            .beta_key
            .as_deref()
            .is_some_and(|expected| bool::from(expected.ct_eq(provided)));

        match (production_match, beta_match) {
            (true, false) => Some(RoutingProfile::Production),
            (false, true) => Some(RoutingProfile::Beta),
            (false, false) | (true, true) => None,
        }
    }
}

pub fn protect(router: Router, auth: GatewayAuth) -> Router {
    router.layer(middleware::from_fn_with_state(auth, authorize))
}

async fn authorize(State(auth): State<GatewayAuth>, mut request: Request, next: Next) -> Response {
    let profile = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|provided| auth.profile_for(provided.as_bytes()));

    if let Some(profile) = profile {
        request.extensions_mut().insert(profile);
        next.run(request).await
    } else {
        unauthorized()
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": {
                "message": "unauthorized"
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::Extension;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    use super::*;

    const TEST_KEY: &str = "expected-gateway-key";
    const BETA_KEY: &str = "expected-beta-key";

    fn app() -> Router {
        protect(
            Router::new().route(
                "/protected",
                get(|Extension(profile): Extension<RoutingProfile>| async move {
                    match profile {
                        RoutingProfile::Production => StatusCode::NO_CONTENT,
                        RoutingProfile::Beta => StatusCode::ACCEPTED,
                    }
                }),
            ),
            GatewayAuth::new(TEST_KEY).with_beta_key(BETA_KEY),
        )
    }

    async fn request(authorization: Option<&str>) -> Response {
        let mut request = Request::builder().uri("/protected");
        if let Some(value) = authorization {
            request = request.header(AUTHORIZATION, value);
        }
        app()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn assert_unauthorized(response: Response) {
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({
                "error": {
                    "message": "unauthorized"
                }
            })
        );
    }

    #[tokio::test]
    async fn correct_bearer_reaches_handler() {
        let response = request(Some("Bearer expected-gateway-key")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn beta_bearer_attaches_beta_profile() {
        let response = request(Some("Bearer expected-beta-key")).await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn forged_profile_header_cannot_change_the_credential_profile() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(AUTHORIZATION, "Bearer expected-gateway-key")
                    .header("x-seren-routing-profile", "beta")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn missing_bearer_is_rejected() {
        assert_unauthorized(request(None).await).await;
    }

    #[tokio::test]
    async fn wrong_bearer_is_rejected() {
        assert_unauthorized(request(Some("Bearer wrong-gateway-key")).await).await;
    }
}
