// ABOUTME: Persists exact served-provider cost for reconciliation.
// ABOUTME: Serves OpenRouter-shaped generation metadata by provider response ID.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use jiff_sqlx::Timestamp;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{Number, Value, json};
use sqlx::{FromRow, PgPool};

use crate::db::DatabaseHealth;
use crate::registry::Registry;
use crate::routing_profile::RoutingProfile;

const LEDGER_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct Ledger {
    pool: PgPool,
    database_health: DatabaseHealth,
    public_provider_aliases: Arc<BTreeMap<String, String>>,
}

impl Ledger {
    pub fn new(pool: PgPool) -> Self {
        Self::with_health(pool, DatabaseHealth::ready())
    }

    pub fn with_health(pool: PgPool, database_health: DatabaseHealth) -> Self {
        Self {
            pool,
            database_health,
            public_provider_aliases: Arc::default(),
        }
    }

    pub fn with_public_provider_aliases(mut self, registry: &Registry) -> Self {
        self.public_provider_aliases = Arc::new(
            registry
                .providers
                .iter()
                .filter_map(|provider| {
                    match (&provider.public_display_name, &provider.public_tag) {
                        (Some(display_name), Some(_)) => {
                            Some((provider.id.clone(), display_name.clone()))
                        }
                        _ => None,
                    }
                })
                .collect(),
        );
        self
    }

    fn public_provider_name<'a>(&'a self, provider_id: &'a str) -> &'a str {
        self.public_provider_aliases
            .get(provider_id)
            .map(String::as_str)
            .unwrap_or(provider_id)
    }

    pub(crate) async fn insert(&self, generation: &GenerationWrite) -> Result<(), sqlx::Error> {
        let result = tokio::time::timeout(
            LEDGER_OPERATION_TIMEOUT,
            sqlx::query(
                r#"
                INSERT INTO generations (
                    id,
                    routing_profile,
                    canonical_slug,
                    provider_id,
                    prompt_tokens,
                    completion_tokens,
                    cost_usd,
                    provider_cost_usd,
                    sell_price_usd,
                    latency_ms,
                    status
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                "#,
            )
            .bind(&generation.id)
            .bind(generation.routing_profile.as_str())
            .bind(&generation.canonical_slug)
            .bind(&generation.provider_id)
            .bind(generation.prompt_tokens)
            .bind(generation.completion_tokens)
            // The historical sell_price_usd column stays null for new writes.
            // cost_usd and provider_cost_usd both hold the metered provider cost.
            .bind(generation.cost_usd)
            .bind(generation.cost_usd)
            .bind(Option::<Decimal>::None)
            .bind(generation.latency_ms)
            .bind(generation.status)
            .execute(&self.pool),
        )
        .await
        .unwrap_or(Err(sqlx::Error::PoolTimedOut));

        match result {
            Ok(_) => {
                self.database_health.report_ready();
                Ok(())
            }
            Err(error) => {
                self.database_health
                    .report_failure("insert_generation", &error);
                Err(error)
            }
        }
    }

    async fn fetch(
        &self,
        id: &str,
        routing_profile: RoutingProfile,
    ) -> Result<Option<Generation>, sqlx::Error> {
        let result = tokio::time::timeout(
            LEDGER_OPERATION_TIMEOUT,
            sqlx::query_as::<_, Generation>(
                r#"
                SELECT
                    id,
                    created_at,
                    canonical_slug,
                    provider_id,
                    prompt_tokens,
                    completion_tokens,
                    CASE
                        WHEN sell_price_usd IS NOT NULL THEN sell_price_usd
                        ELSE COALESCE(provider_cost_usd, cost_usd)
                    END AS reported_cost_usd,
                    provider_cost_usd,
                    latency_ms
                FROM generations
                WHERE id = $1
                  AND routing_profile = $2
                "#,
            )
            .bind(id)
            .bind(routing_profile.as_str())
            .fetch_optional(&self.pool),
        )
        .await
        .unwrap_or(Err(sqlx::Error::PoolTimedOut));

        match result {
            Ok(generation) => {
                self.database_health.report_ready();
                Ok(generation)
            }
            Err(error) => {
                self.database_health
                    .report_failure("fetch_generation", &error);
                Err(error)
            }
        }
    }
}

pub(crate) struct GenerationWrite {
    pub(crate) id: String,
    pub(crate) routing_profile: RoutingProfile,
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
    reported_cost_usd: Option<Decimal>,
    provider_cost_usd: Option<Decimal>,
    latency_ms: i64,
}

#[derive(Deserialize)]
pub(crate) struct GenerationQuery {
    id: String,
}

pub(crate) async fn get_generation(
    State(ledger): State<Ledger>,
    Extension(routing_profile): Extension<RoutingProfile>,
    query: Result<Query<GenerationQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(query) if !query.id.trim().is_empty() => query,
        Ok(_) | Err(_) => return api_error(StatusCode::BAD_REQUEST, "id is required"),
    };

    match ledger.fetch(&query.id, routing_profile).await {
        Ok(Some(generation)) => {
            let provider_name = ledger
                .public_provider_name(&generation.provider_id)
                .to_owned();
            generation_response(generation, provider_name)
        }
        Ok(None) => api_error(StatusCode::NOT_FOUND, "generation not found"),
        Err(error) => {
            tracing::error!(
                error = %error,
                generation_id = query.id,
                "generation lookup failed"
            );
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "generation ledger unavailable",
            )
        }
    }
}

fn generation_response(generation: Generation, provider_name: String) -> Response {
    let total_cost = generation
        .reported_cost_usd
        .map(decimal_value)
        .unwrap_or(Value::Null);

    Json(json!({
        "data": {
            "id": generation.id,
            "created_at": generation.created_at.to_jiff().to_string(),
            "model": generation.canonical_slug,
            "provider_name": provider_name,
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
        let mut registry: Registry =
            serde_yaml::from_str(include_str!("../tests/fixtures/catalog_registry.yaml")).unwrap();
        registry.providers[0].id = "provider-a".to_owned();
        registry.providers[0].public_display_name = Some("Seren Inference".to_owned());
        registry.providers[0].public_tag = Some("seren".to_owned());
        registry.validate().unwrap();
        let ledger = Ledger::new(pool.clone()).with_public_provider_aliases(&registry);
        let write = GenerationWrite {
            id: "chatcmpl-ledger-round-trip".to_owned(),
            routing_profile: RoutingProfile::Production,
            canonical_slug: "canonical/model".to_owned(),
            provider_id: "provider-a".to_owned(),
            prompt_tokens: 12,
            completion_tokens: 5,
            cost_usd: "0.0000150000".parse().unwrap(),
            latency_ms: 321,
            status: 200,
        };

        ledger.insert(&write).await.unwrap();
        let stored = ledger
            .fetch(&write.id, RoutingProfile::Production)
            .await
            .unwrap()
            .unwrap();
        assert!(
            ledger
                .fetch(&write.id, RoutingProfile::Beta)
                .await
                .unwrap()
                .is_none()
        );

        assert_eq!(stored.id, write.id);
        assert_eq!(stored.canonical_slug, write.canonical_slug);
        assert_eq!(stored.provider_id, write.provider_id);
        assert_eq!(stored.prompt_tokens, Some(write.prompt_tokens));
        assert_eq!(stored.completion_tokens, Some(write.completion_tokens));
        assert_eq!(stored.provider_cost_usd, Some(write.cost_usd));
        assert_eq!(stored.reported_cost_usd, Some(write.cost_usd));
        assert_eq!(
            sqlx::query_scalar::<_, Decimal>("SELECT cost_usd FROM generations WHERE id = $1")
                .bind(&write.id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            write.cost_usd
        );
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
            .with_state(ledger.clone())
            .layer(Extension(RoutingProfile::Production));
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
        assert_eq!(found["data"]["provider_name"], "Seren Inference");
        assert_eq!(found["data"]["tokens_prompt"], write.prompt_tokens);
        assert_eq!(found["data"]["tokens_completion"], write.completion_tokens);
        assert_eq!(
            found["data"]["total_cost"].as_number().unwrap().to_string(),
            "0.0000150000"
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

        sqlx::query(
            r#"
            INSERT INTO generations (
                id,
                routing_profile,
                canonical_slug,
                provider_id,
                prompt_tokens,
                completion_tokens,
                cost_usd,
                latency_ms,
                status
            )
            VALUES ($1, 'production', 'canonical/model', 'legacy-provider', 1, 1, $2, 1, 200)
            "#,
        )
        .bind("chatcmpl-legacy-rollback")
        .bind(Decimal::new(7, 10))
        .execute(&pool)
        .await
        .unwrap();
        let legacy = ledger
            .fetch("chatcmpl-legacy-rollback", RoutingProfile::Production)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(legacy.reported_cost_usd, Some(Decimal::new(7, 10)));
        assert_eq!(legacy.provider_cost_usd, None);
    }

    #[sqlx::test]
    async fn historical_sell_row_preserves_its_reported_cost(pool: PgPool) {
        let ledger = Ledger::new(pool.clone());
        sqlx::query(
            r#"
            INSERT INTO generations (
                id,
                routing_profile,
                canonical_slug,
                provider_id,
                prompt_tokens,
                completion_tokens,
                cost_usd,
                provider_cost_usd,
                sell_price_usd,
                latency_ms,
                status
            )
            VALUES (
                'chatcmpl-historical-sell',
                'beta',
                'canonical/model',
                'historical-provider',
                1000,
                20,
                0.0033000000,
                0.0044000000,
                0.0033000000,
                456,
                200
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let stored = ledger
            .fetch("chatcmpl-historical-sell", RoutingProfile::Beta)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.provider_cost_usd,
            Some("0.0044000000".parse().unwrap())
        );
        assert_eq!(
            stored.reported_cost_usd,
            Some("0.0033000000".parse().unwrap())
        );
    }
}
