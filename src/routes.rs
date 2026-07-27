//! Shared public and authenticated route registries.
//!
//! Keep route definitions here so the production server and tests exercise the
//! same surface. Apply authentication only to [`protected_router`].

use axum::Router;
use axum::routing::{get, post};

use crate::catalog::{self, Catalog};
use crate::compatibility;
use crate::gateway_auth::{self, GatewayAuth};
use crate::ledger::{self, Ledger};
use crate::proxy::{self, ProxyState};
use crate::server::{inference_route_policies, standard_route_policies};

pub fn public_router() -> Router {
    standard_route_policies(Router::new().route("/", get(hello)))
}

pub fn protected_router(
    auth: GatewayAuth,
    proxy_state: ProxyState,
    ledger: Ledger,
    catalog: Catalog,
) -> Router {
    let inference = Router::new()
        .route("/api/v1/chat/completions", post(proxy::chat_completions))
        .route("/api/v1/completions", post(proxy::legacy_completions))
        .with_state(proxy_state);
    let generation = Router::new()
        .route("/api/v1/generation", get(ledger::get_generation))
        .with_state(ledger);
    let models = catalog_router(catalog);
    let compatibility = Router::new()
        .route("/api/v1/auth/key", get(compatibility::get_auth_key))
        .route("/api/v1/credits", get(compatibility::get_credits));
    let router = inference_route_policies(inference)
        .merge(standard_route_policies(generation))
        .merge(standard_route_policies(models))
        .merge(standard_route_policies(compatibility));

    gateway_auth::protect(router, auth)
}

fn catalog_router(catalog: Catalog) -> Router {
    Router::new()
        .route("/api/v1/models", get(catalog::get_models))
        .route(
            "/api/v1/models/{author}/{slug}/endpoints",
            get(catalog::get_model_endpoints_by_author),
        )
        .route(
            "/api/v1/models/{model}/endpoints",
            get(catalog::get_model_endpoints),
        )
        .with_state(catalog)
}

async fn hello() -> &'static str {
    "Hello from Seren!"
}

#[cfg(test)]
mod tests {
    use axum::Extension;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::{catalog_router, public_router};
    use crate::catalog::Catalog;
    use crate::registry::Registry;
    use crate::routing_profile::RoutingProfile;

    #[tokio::test]
    async fn public_router_serves_root() {
        let response = public_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn production_catalog_routes_match_for_canonical_and_encoded_model_paths() {
        let registry: Registry =
            serde_yaml::from_str(include_str!("../tests/fixtures/catalog_registry.yaml")).unwrap();
        let app = catalog_router(Catalog::from_registry(&registry))
            .layer(Extension(RoutingProfile::Production));

        let canonical = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models/anthropic/claude-opus-5-fast/endpoints")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let encoded = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models/anthropic%2Fclaude-opus-5-fast/endpoints")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(canonical.status(), StatusCode::OK);
        assert_eq!(encoded.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(canonical.into_body(), usize::MAX).await.unwrap(),
            to_bytes(encoded.into_body(), usize::MAX).await.unwrap()
        );

        let unknown = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models/unknown%2Fmodel/endpoints")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            serde_json::from_slice::<Value>(
                &to_bytes(unknown.into_body(), usize::MAX).await.unwrap()
            )
            .unwrap(),
            json!({"error": {"code": 404, "message": "Not Found"}})
        );
    }
}
