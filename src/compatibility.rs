// ABOUTME: Serves fixed OpenRouter-compatible metadata responses required by Gateway probes.
// ABOUTME: Keeps account and credit ownership in the Gateway instead of this routing service.

use axum::Json;
use axum::response::IntoResponse;
use serde::Serialize;

#[derive(Debug, Eq, PartialEq, Serialize)]
struct AuthKeyResponse {
    data: AuthKeyData,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct AuthKeyData {
    label: &'static str,
    limit: Option<u64>,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct CreditsResponse {
    data: CreditsData,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct CreditsData {
    total_credits: u64,
    total_usage: u64,
}

pub async fn get_auth_key() -> impl IntoResponse {
    Json(AuthKeyResponse {
        data: AuthKeyData {
            label: "seren-router",
            limit: None,
        },
    })
}

pub async fn get_credits() -> impl IntoResponse {
    Json(CreditsResponse {
        data: CreditsData {
            total_credits: 0,
            total_usage: 0,
        },
    })
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use serde_json::{Value, json};

    use super::*;

    async fn response_json(response: impl IntoResponse) -> Value {
        let response = response.into_response();
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn auth_key_response_is_exact() {
        assert_eq!(
            response_json(get_auth_key().await).await,
            json!({"data": {"label": "seren-router", "limit": null}})
        );
    }

    #[tokio::test]
    async fn credits_response_is_exact() {
        assert_eq!(
            response_json(get_credits().await).await,
            json!({"data": {"total_credits": 0, "total_usage": 0}})
        );
    }
}
