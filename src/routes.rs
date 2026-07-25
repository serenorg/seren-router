//! Shared public and authenticated route registries.
//!
//! Keep route definitions here so the production server and tests exercise the
//! same surface. Apply authentication only to [`protected_router`].

use axum::Router;
use axum::routing::{get, post};

use crate::gateway_auth::{self, GatewayAuth};
use crate::proxy::{self, ProxyState};
use crate::server::{inference_route_policies, standard_route_policies};

pub fn public_router() -> Router {
    standard_route_policies(Router::new().route("/", get(hello)))
}

pub fn protected_router(auth: GatewayAuth, proxy_state: ProxyState) -> Router {
    let router = Router::new()
        .route("/api/v1/chat/completions", post(proxy::chat_completions))
        .route("/api/v1/completions", post(proxy::legacy_completions))
        .with_state(proxy_state);

    gateway_auth::protect(inference_route_policies(router), auth)
}

async fn hello() -> &'static str {
    "Hello from Seren!"
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::public_router;

    #[tokio::test]
    async fn public_router_serves_root() {
        let response = public_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
