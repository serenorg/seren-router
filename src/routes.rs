//! Shared public and authenticated route registries.
//!
//! Keep route definitions here so the production server and tests exercise the
//! same surface. Apply authentication only to [`protected_router`].

use axum::routing::get;
use axum::{Json, Router};

use crate::auth::SerenIdentity;
use crate::server::{inference_route_policies, standard_route_policies};

pub fn public_router() -> Router {
    standard_route_policies(Router::new().route("/", get(hello)))
}

pub fn protected_router() -> Router {
    let standard = standard_route_policies(Router::new().route("/whoami", get(whoami)));

    // Inference endpoints are registered here in M2. Keeping their policy boundary
    // inside the shared protected builder prevents production and tests from
    // drifting into parallel route lists.
    let inference = inference_route_policies(Router::new());

    standard.merge(inference)
}

async fn hello() -> &'static str {
    "Hello from Seren!"
}

async fn whoami(identity: SerenIdentity) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "user_id": identity.user_id,
        "organization_id": identity.organization_id,
        "agent_wallet": identity.agent_wallet,
    }))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::{protected_router, public_router};

    #[tokio::test]
    async fn public_router_serves_root() {
        let response = public_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn protected_router_fails_closed_without_auth_middleware() {
        let response = protected_router()
            .oneshot(
                Request::builder()
                    .uri("/whoami")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
