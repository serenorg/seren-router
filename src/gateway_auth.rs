// ABOUTME: Authenticates the Seren Gateway's static bearer credential.
// ABOUTME: Compares key bytes in constant time and never logs presented credentials.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header::AUTHORIZATION};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use subtle::ConstantTimeEq;

#[derive(Clone)]
pub struct GatewayAuth {
    expected_key: Arc<[u8]>,
}

impl GatewayAuth {
    pub fn new(expected_key: impl AsRef<[u8]>) -> Self {
        Self {
            expected_key: Arc::from(expected_key.as_ref()),
        }
    }
}

pub fn protect(router: Router, auth: GatewayAuth) -> Router {
    router.layer(middleware::from_fn_with_state(auth, authorize))
}

async fn authorize(State(auth): State<GatewayAuth>, request: Request, next: Next) -> Response {
    let authorized = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|provided| bool::from(auth.expected_key.as_ref().ct_eq(provided.as_bytes())));

    if authorized {
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
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    use super::*;

    const TEST_KEY: &str = "expected-gateway-key";

    fn app() -> Router {
        protect(
            Router::new().route("/protected", get(|| async { StatusCode::NO_CONTENT })),
            GatewayAuth::new(TEST_KEY),
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
    async fn missing_bearer_is_rejected() {
        assert_unauthorized(request(None).await).await;
    }

    #[tokio::test]
    async fn wrong_bearer_is_rejected() {
        assert_unauthorized(request(Some("Bearer wrong-gateway-key")).await).await;
    }
}
