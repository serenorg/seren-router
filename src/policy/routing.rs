// ABOUTME: Composes registry routes, live measurements, and deterministic selection per request.
// ABOUTME: Owns process-local RNG and rolling traffic share without performing network work.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde_json::Value;
use thiserror::Error;

use crate::config::RoutingConfig;
use crate::registry::Registry;
use crate::routing_profile::RoutingProfile;

use super::measurements::MeasurementStore;
use super::preference::{PreferenceError, parse_request};
use super::select::{Candidate, CandidateError, ShareTracker, select_route};

#[derive(Clone)]
pub struct RoutingPolicy {
    routes: Arc<HashMap<(RoutingProfile, String), Vec<RouteDescriptor>>>,
    config: RoutingConfig,
    measurements: MeasurementStore,
    state: Arc<Mutex<RoutingState>>,
}

#[derive(Clone, Debug)]
struct RouteDescriptor {
    provider_id: String,
    input_price_per_mtok: rust_decimal::Decimal,
    output_price_per_mtok: rust_decimal::Decimal,
    priority: u8,
}

struct RoutingState {
    shares: HashMap<(RoutingProfile, String), ShareTracker>,
    rng: StdRng,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteDecision {
    pub profile: RoutingProfile,
    pub canonical_model: String,
    pub selected_provider: String,
    pub fallback_model: String,
    pub has_alternatives: bool,
}

impl RoutingPolicy {
    pub fn from_registry(
        registry: &Registry,
        config: RoutingConfig,
        measurements: MeasurementStore,
    ) -> Result<Self, RoutingPolicyError> {
        let mut operating_system_rng = rand::rng();
        Self::from_registry_with_rng(
            registry,
            config,
            measurements,
            StdRng::from_rng(&mut operating_system_rng),
        )
    }

    fn from_registry_with_rng(
        registry: &Registry,
        config: RoutingConfig,
        measurements: MeasurementStore,
        rng: StdRng,
    ) -> Result<Self, RoutingPolicyError> {
        let mut routes = HashMap::<(RoutingProfile, String), Vec<RouteDescriptor>>::new();
        for provider in registry
            .providers
            .iter()
            .filter(|provider| provider.enabled)
        {
            for model in &provider.models {
                Candidate::new(
                    &provider.id,
                    model.input_price_per_mtok,
                    model.output_price_per_mtok,
                    provider.priority,
                    None,
                    true,
                )?;
                for profile in provider.profiles.iter().copied() {
                    routes
                        .entry((profile, model.slug.clone()))
                        .or_default()
                        .push(RouteDescriptor {
                            provider_id: provider.id.clone(),
                            input_price_per_mtok: model.input_price_per_mtok,
                            output_price_per_mtok: model.output_price_per_mtok,
                            priority: provider.priority,
                        });
                }
            }
        }
        for candidates in routes.values_mut() {
            candidates.sort_by(|left, right| {
                left.priority
                    .cmp(&right.priority)
                    .then_with(|| left.provider_id.cmp(&right.provider_id))
            });
        }

        Ok(Self {
            routes: Arc::new(routes),
            config,
            measurements,
            state: Arc::new(Mutex::new(RoutingState {
                shares: HashMap::new(),
                rng,
            })),
        })
    }

    pub fn route(
        &self,
        profile: RoutingProfile,
        request: &mut Value,
    ) -> Result<RouteDecision, RouteRequestError> {
        reject_provider_overrides(request)?;
        let parsed = parse_request(request)?;
        consume_routing_sort(request);
        let route_key = (profile, parsed.canonical_model.clone());
        let descriptors = self
            .routes
            .get(&route_key)
            .ok_or(RouteRequestError::UnknownModel)?;
        let candidates: Vec<_> = descriptors
            .iter()
            .map(|route| {
                Candidate::new(
                    &route.provider_id,
                    route.input_price_per_mtok,
                    route.output_price_per_mtok,
                    route.priority,
                    self.measurements
                        .get_for(profile, &route.provider_id, &parsed.canonical_model),
                    true,
                )
                .expect("registry candidates were validated during routing-policy construction")
            })
            .collect();

        let mut state = self.state.lock().expect("routing state lock poisoned");
        let mut recent_share = state.shares.remove(&route_key).unwrap_or_else(|| {
            ShareTracker::new(self.config.share_window())
                .expect("routing config validates the share window")
        });
        let Some(selected) = select_route(
            &candidates,
            parsed.preference,
            &self.config.policy(),
            &recent_share,
            &mut state.rng,
        ) else {
            state.shares.insert(route_key, recent_share);
            return Err(RouteRequestError::NoEligibleRoute);
        };
        let selected_provider = selected.provider_id().to_owned();
        recent_share.record(selected_provider.clone());
        state.shares.insert(route_key, recent_share);
        drop(state);

        request["model"] = Value::String(format!("{selected_provider}/{}", parsed.canonical_model));

        Ok(RouteDecision {
            profile,
            fallback_model: profile.sidecar_alias(&parsed.canonical_model),
            canonical_model: parsed.canonical_model,
            selected_provider,
            has_alternatives: candidates.len() > 1,
        })
    }

    pub fn measurements(&self) -> MeasurementStore {
        self.measurements.clone()
    }
}

fn consume_routing_sort(request: &mut Value) {
    let Some(request) = request.as_object_mut() else {
        return;
    };
    let remove_provider = request
        .get_mut("provider")
        .and_then(Value::as_object_mut)
        .is_some_and(|provider| {
            provider.remove("sort");
            provider.is_empty()
        });
    if remove_provider {
        request.remove("provider");
    }
}

fn reject_provider_overrides(request: &Value) -> Result<(), RouteRequestError> {
    let Some(provider) = request.get("provider").and_then(Value::as_object) else {
        return Ok(());
    };
    if ["only", "ignore", "order"]
        .into_iter()
        .any(|field| provider.contains_key(field))
    {
        return Err(RouteRequestError::UnsupportedProviderOverride);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum RoutingPolicyError {
    #[error(transparent)]
    InvalidCandidate(#[from] CandidateError),
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RouteRequestError {
    #[error(transparent)]
    InvalidPreference(#[from] PreferenceError),
    #[error("model not found")]
    UnknownModel,
    #[error("no eligible provider route")]
    NoEligibleRoute,
    #[error("provider.only, provider.ignore, and provider.order are not supported")]
    UnsupportedProviderOverride,
}

impl IntoResponse for RouteRequestError {
    fn into_response(self) -> Response {
        if let Self::InvalidPreference(error) = self {
            return error.into_response();
        }
        let status = match self {
            Self::UnknownModel => StatusCode::NOT_FOUND,
            Self::NoEligibleRoute => StatusCode::SERVICE_UNAVAILABLE,
            Self::UnsupportedProviderOverride => StatusCode::BAD_REQUEST,
            Self::InvalidPreference(_) => unreachable!("handled above"),
        };

        (
            status,
            Json(serde_json::json!({
                "error": {
                    "code": status.as_u16(),
                    "message": self.to_string()
                }
            })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use axum::body::to_bytes;
    use rand::SeedableRng;
    use rust_decimal::Decimal;
    use serde_json::json;

    use super::*;
    use crate::policy::measurements::{MeasurementStore, Observation};
    use crate::registry::{ModelMapping, Provider};

    #[test]
    fn request_routing_uses_live_measurements_and_consumes_sort() {
        let measurements = MeasurementStore::default();
        measurements
            .observe(
                "fast",
                "vendor/model",
                Observation {
                    completion_tokens: 100,
                    stream_duration: std::time::Duration::from_secs(1),
                    time_to_first_token: std::time::Duration::from_millis(100),
                },
            )
            .unwrap();
        let routing = test_routing(measurements);
        let mut request = json!({
            "model": "vendor/model:nitro",
            "provider": {"sort": "throughput"},
            "reasoning": {"effort": "high"}
        });

        let decision = routing
            .route(RoutingProfile::Production, &mut request)
            .unwrap();

        assert_eq!(
            decision,
            RouteDecision {
                profile: RoutingProfile::Production,
                canonical_model: "vendor/model".to_owned(),
                selected_provider: "fast".to_owned(),
                fallback_model: "seren-profile-production/vendor/model".to_owned(),
                has_alternatives: true,
            }
        );
        assert_eq!(request["model"], "fast/vendor/model");
        assert!(request.get("provider").is_none());
        assert_eq!(request["reasoning"], json!({"effort": "high"}));
    }

    #[test]
    fn caller_provider_overrides_are_rejected_before_routing() {
        let routing = test_routing(MeasurementStore::default());

        for field in ["only", "ignore", "order"] {
            let mut request = json!({
                "model": "vendor/model",
                "provider": {field: ["fast"]}
            });

            assert_eq!(
                routing
                    .route(RoutingProfile::Production, &mut request)
                    .unwrap_err(),
                RouteRequestError::UnsupportedProviderOverride
            );
            assert_eq!(request["model"], "vendor/model");
        }
    }

    #[tokio::test]
    async fn unknown_models_fail_locally_with_json_not_found() {
        let routing = test_routing(MeasurementStore::default());
        let mut request = json!({"model": "unknown/model"});

        let response = routing
            .route(RoutingProfile::Production, &mut request)
            .unwrap_err()
            .into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            serde_json::from_slice::<Value>(
                &to_bytes(response.into_body(), usize::MAX).await.unwrap()
            )
            .unwrap(),
            json!({"error": {"code": 404, "message": "model not found"}})
        );
    }

    #[test]
    fn provider_selection_and_share_state_are_profile_scoped() {
        let registry = Registry {
            providers: vec![
                provider_for("production-only", 0, "1.0", [RoutingProfile::Production]),
                provider_for("beta-only", 0, "1.0", [RoutingProfile::Beta]),
            ],
        };
        let routing = RoutingPolicy::from_registry_with_rng(
            &registry,
            RoutingConfig::new("10".parse().unwrap(), 0.1, Decimal::ONE, 100).unwrap(),
            MeasurementStore::default(),
            StdRng::seed_from_u64(7),
        )
        .unwrap();
        let mut production = json!({"model": "vendor/model"});
        let mut beta = production.clone();

        let production_decision = routing
            .route(RoutingProfile::Production, &mut production)
            .unwrap();
        let beta_decision = routing.route(RoutingProfile::Beta, &mut beta).unwrap();

        assert_eq!(production_decision.selected_provider, "production-only");
        assert_eq!(beta_decision.selected_provider, "beta-only");
        assert_eq!(production["model"], "production-only/vendor/model");
        assert_eq!(beta["model"], "beta-only/vendor/model");
    }

    fn test_routing(measurements: MeasurementStore) -> RoutingPolicy {
        let mut disabled = provider("disabled", 0, "0.1");
        disabled.enabled = false;
        let registry = Registry {
            providers: vec![
                disabled,
                provider("cheap", 1, "1.0"),
                provider("fast", 2, "2.0"),
            ],
        };
        let config = RoutingConfig::new("10".parse().unwrap(), 0.1, Decimal::ONE, 100).unwrap();

        RoutingPolicy::from_registry_with_rng(
            &registry,
            config,
            measurements,
            StdRng::seed_from_u64(7),
        )
        .unwrap()
    }

    fn provider(id: &str, priority: u8, price: &str) -> Provider {
        provider_for(id, priority, price, [RoutingProfile::Production])
    }

    fn provider_for(
        id: &str,
        priority: u8,
        price: &str,
        profiles: impl IntoIterator<Item = RoutingProfile>,
    ) -> Provider {
        Provider {
            id: id.to_owned(),
            display_name: id.to_owned(),
            base_url: "http://127.0.0.1:1234/v1".to_owned(),
            secret_env: format!("KEY_{}", id.to_uppercase()),
            enabled: true,
            priority,
            profiles: BTreeSet::from_iter(profiles),
            models: vec![ModelMapping {
                slug: "vendor/model".to_owned(),
                name: "Model".to_owned(),
                context_length: 1,
                provider_model_id: "upstream".to_owned(),
                input_price_per_mtok: price.parse().unwrap(),
                output_price_per_mtok: Decimal::ZERO,
            }],
        }
    }
}
