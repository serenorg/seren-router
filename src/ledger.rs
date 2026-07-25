// ABOUTME: Persists successful provider generations for exact cost reconciliation.
// ABOUTME: Serves OpenRouter-shaped generation metadata by provider response ID.

use std::str::FromStr;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use jiff_sqlx::Timestamp;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{Number, Value, json};
use sqlx::{FromRow, PgPool};

#[derive(Clone)]
pub struct Ledger {
    pool: PgPool,
}

impl Ledger {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) async fn insert(&self, generation: &GenerationWrite) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO generations (
                id,
                canonical_slug,
                provider_id,
                prompt_tokens,
                completion_tokens,
                cost_usd,
                latency_ms,
                status
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(&generation.id)
        .bind(&generation.canonical_slug)
        .bind(&generation.provider_id)
        .bind(generation.prompt_tokens)
        .bind(generation.completion_tokens)
        .bind(generation.cost_usd)
        .bind(generation.latency_ms)
        .bind(generation.status)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn fetch(&self, id: &str) -> Result<Option<Generation>, sqlx::Error> {
        sqlx::query_as::<_, Generation>(
            r#"
            SELECT
                id,
                created_at,
                canonical_slug,
                provider_id,
                prompt_tokens,
                completion_tokens,
                cost_usd,
                latency_ms
            FROM generations
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }
}

pub(crate) struct GenerationWrite {
    pub(crate) id: String,
    pub(crate) canonical_slug: String,
    pub(crate) provider_id: String,
    pub(crate) prompt_tokens: i64,
    pub(crate) completion_tokens: i64,
    pub(crate) cost_usd: Decimal,
    pub(crate) latency_ms: i64,
    pub(crate) status: i16,
}

#[derive(Debug, Eq, FromRow, PartialEq)]
struct Generation {
    id: String,
    created_at: Timestamp,
    canonical_slug: String,
    provider_id: String,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    cost_usd: Option<Decimal>,
    latency_ms: i64,
}

#[derive(Deserialize)]
pub(crate) struct GenerationQuery {
    id: String,
}

pub(crate) async fn get_generation(
    State(ledger): State<Ledger>,
    query: Result<Query<GenerationQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(query) if !query.id.trim().is_empty() => query,
        Ok(_) | Err(_) => return api_error(StatusCode::BAD_REQUEST, "id is required"),
    };

    match ledger.fetch(&query.id).await {
        Ok(Some(generation)) => generation_response(generation),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "generation not found"),
        Err(error) => {
            tracing::error!(
                error = %error,
                generation_id = query.id,
                "generation lookup failed"
            );
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "generation lookup failed",
            )
        }
    }
}

fn generation_response(generation: Generation) -> Response {
    let total_cost = generation
        .cost_usd
        .map(decimal_value)
        .unwrap_or(Value::Null);

    Json(json!({
        "data": {
            "id": generation.id,
            "created_at": generation.created_at.to_jiff().to_string(),
            "model": generation.canonical_slug,
            "provider_name": generation.provider_id,
            "tokens_prompt": generation.prompt_tokens,
            "tokens_completion": generation.completion_tokens,
            "total_cost": total_cost,
            "latency": generation.latency_ms
        }
    }))
    .into_response()
}

fn decimal_value(value: Decimal) -> Value {
    Value::Number(
        Number::from_str(&value.to_string()).expect("Decimal always serializes as a JSON number"),
    )
}

fn api_error(status: StatusCode, message: &'static str) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": message
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;

    use super::*;

    #[sqlx::test]
    async fn generation_round_trip_and_unknown_lookup(pool: PgPool) {
        let ledger = Ledger::new(pool.clone());
        let write = GenerationWrite {
            id: "chatcmpl-ledger-round-trip".to_owned(),
            canonical_slug: "canonical/model".to_owned(),
            provider_id: "provider-a".to_owned(),
            prompt_tokens: 12,
            completion_tokens: 5,
            cost_usd: "0.0000190000".parse().unwrap(),
            latency_ms: 321,
            status: 200,
        };

        ledger.insert(&write).await.unwrap();
        let stored = ledger.fetch(&write.id).await.unwrap().unwrap();

        assert_eq!(stored.id, write.id);
        assert_eq!(stored.canonical_slug, write.canonical_slug);
        assert_eq!(stored.provider_id, write.provider_id);
        assert_eq!(stored.prompt_tokens, Some(write.prompt_tokens));
        assert_eq!(stored.completion_tokens, Some(write.completion_tokens));
        assert_eq!(stored.cost_usd, Some(write.cost_usd));
        assert_eq!(stored.latency_ms, write.latency_ms);
        assert_eq!(
            sqlx::query_scalar::<_, i16>("SELECT status FROM generations WHERE id = $1")
                .bind(&write.id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            write.status
        );

        let app = axum::Router::new()
            .route("/api/v1/generation", get(get_generation))
            .with_state(ledger);
        let found = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/generation?id={}", write.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(found.status(), StatusCode::OK);
        let found: Value =
            serde_json::from_slice(&to_bytes(found.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(found["data"]["id"], write.id);
        assert_eq!(found["data"]["model"], write.canonical_slug);
        assert_eq!(found["data"]["provider_name"], write.provider_id);
        assert_eq!(found["data"]["tokens_prompt"], write.prompt_tokens);
        assert_eq!(found["data"]["tokens_completion"], write.completion_tokens);
        assert_eq!(
            found["data"]["total_cost"].as_number().unwrap().to_string(),
            "0.0000190000"
        );
        assert!(found["data"]["created_at"].as_str().is_some());
        assert_eq!(found["data"]["latency"], write.latency_ms);

        let response = app
            .oneshot(
                Request::get("/api/v1/generation?id=unknown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            serde_json::from_slice::<Value>(
                &to_bytes(response.into_body(), usize::MAX).await.unwrap()
            )
            .unwrap(),
            json!({"error": {"message": "generation not found"}})
        );
    }
}
