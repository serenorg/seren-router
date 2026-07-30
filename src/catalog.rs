// ABOUTME: Builds OpenRouter-shaped model and endpoint catalogs from enabled registry mappings.
// ABOUTME: Serves deterministic snapshots with exact provider prices.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::registry::{ModelMapping, Provider, Registry};
use crate::routing_profile::RoutingProfile;

const TOKENS_PER_MILLION: u64 = 1_000_000;

#[derive(Clone, Debug)]
pub struct Catalog {
    snapshots: Arc<BTreeMap<RoutingProfile, CatalogSnapshot>>,
}

#[derive(Clone, Debug)]
struct CatalogSnapshot {
    response: Arc<ModelsResponse>,
    endpoints_by_slug: Arc<BTreeMap<String, ModelEndpointsResponse>>,
}

impl Catalog {
    pub fn from_registry(registry: &Registry) -> Self {
        Self {
            snapshots: Arc::new(
                RoutingProfile::ALL
                    .into_iter()
                    .map(|profile| (profile, CatalogSnapshot::from_registry(registry, profile)))
                    .collect(),
            ),
        }
    }

    fn snapshot(&self, profile: RoutingProfile) -> &CatalogSnapshot {
        self.snapshots
            .get(&profile)
            .expect("every routing profile has a catalog snapshot")
    }

    fn endpoint_response(&self, profile: RoutingProfile, slug: &str) -> Response {
        self.snapshot(profile).endpoint_response(slug)
    }
}

impl CatalogSnapshot {
    fn from_registry(registry: &Registry, profile: RoutingProfile) -> Self {
        let mut cheapest_by_slug = BTreeMap::<String, CatalogCandidate<'_>>::new();
        let mut endpoints_by_slug = BTreeMap::<String, Vec<EndpointCandidate<'_>>>::new();

        for provider in registry
            .providers
            .iter()
            .filter(|provider| provider.enabled && provider.supports(profile))
        {
            for mapping in &provider.models {
                let candidate = CatalogCandidate {
                    provider_id: &provider.id,
                    mapping,
                };
                cheapest_by_slug
                    .entry(mapping.slug.clone())
                    .and_modify(|current| {
                        if candidate.is_cheaper_than(current) {
                            *current = candidate;
                        }
                    })
                    .or_insert(candidate);
                endpoints_by_slug
                    .entry(mapping.slug.clone())
                    .or_default()
                    .push(EndpointCandidate { provider, mapping });
            }
        }

        let data: Vec<_> = cheapest_by_slug
            .values()
            .copied()
            .map(CatalogCandidate::into_model)
            .collect();
        let total_count = data.len();
        let endpoints_by_slug = endpoints_by_slug
            .into_iter()
            .map(|(slug, mut candidates)| {
                candidates.sort_by_key(EndpointCandidate::sort_key);
                let selected = cheapest_by_slug
                    .get(&slug)
                    .expect("every endpoint slug has a catalog candidate");
                let endpoints = candidates
                    .into_iter()
                    .map(EndpointCandidate::into_endpoint)
                    .collect();

                (
                    slug.clone(),
                    ModelEndpointsResponse {
                        data: ModelEndpoints {
                            id: slug,
                            name: selected.mapping.name.clone(),
                            endpoints,
                        },
                    },
                )
            })
            .collect();

        Self {
            response: Arc::new(ModelsResponse {
                data,
                links: ModelsLinks { next: None },
                total_count,
            }),
            endpoints_by_slug: Arc::new(endpoints_by_slug),
        }
    }

    fn endpoint_response(&self, slug: &str) -> Response {
        match self.endpoints_by_slug.get(slug) {
            Some(response) => Json(response.clone()).into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: ErrorDetail {
                        code: StatusCode::NOT_FOUND.as_u16(),
                        message: "Not Found",
                    },
                }),
            )
                .into_response(),
        }
    }
}

#[derive(Clone, Copy)]
struct CatalogCandidate<'a> {
    provider_id: &'a str,
    mapping: &'a ModelMapping,
}

impl<'a> CatalogCandidate<'a> {
    fn is_cheaper_than(self, current: &Self) -> bool {
        self.price_key() < current.price_key()
    }

    fn price_key(self) -> (Decimal, Decimal, Decimal, &'a str) {
        (
            self.mapping.input_price_per_mtok + self.mapping.output_price_per_mtok,
            self.mapping.input_price_per_mtok,
            self.mapping.output_price_per_mtok,
            self.provider_id,
        )
    }

    fn into_model(self) -> CatalogModel {
        CatalogModel {
            id: self.mapping.slug.clone(),
            name: self.mapping.name.clone(),
            context_length: self.mapping.context_length,
            pricing: CatalogPricing {
                prompt: per_token_price(self.mapping.input_price_per_mtok),
                completion: per_token_price(self.mapping.output_price_per_mtok),
            },
        }
    }
}

#[derive(Clone, Copy)]
struct EndpointCandidate<'a> {
    provider: &'a Provider,
    mapping: &'a ModelMapping,
}

impl<'a> EndpointCandidate<'a> {
    fn sort_key(&self) -> (u8, &'a str, &'a str) {
        (
            self.provider.priority,
            self.provider.id.as_str(),
            self.mapping.provider_model_id.as_str(),
        )
    }

    fn into_endpoint(self) -> CatalogEndpoint {
        CatalogEndpoint {
            name: format!(
                "{}: {}",
                self.provider.catalog_display_name(),
                self.mapping.name
            ),
            model_id: self.mapping.slug.clone(),
            model_name: self.mapping.name.clone(),
            context_length: self.mapping.context_length,
            pricing: CatalogPricing {
                prompt: per_token_price(self.mapping.input_price_per_mtok),
                completion: per_token_price(self.mapping.output_price_per_mtok),
            },
            provider_name: self.provider.catalog_display_name().to_owned(),
            tag: self.provider.catalog_tag().to_owned(),
        }
    }
}

fn per_token_price(price_per_mtok: Decimal) -> String {
    (price_per_mtok / Decimal::from(TOKENS_PER_MILLION))
        .normalize()
        .to_string()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ModelsResponse {
    data: Vec<CatalogModel>,
    links: ModelsLinks,
    total_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ModelsLinks {
    next: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CatalogModel {
    id: String,
    name: String,
    context_length: u64,
    pricing: CatalogPricing,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CatalogPricing {
    prompt: String,
    completion: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ModelEndpointsResponse {
    data: ModelEndpoints,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ModelEndpoints {
    id: String,
    name: String,
    endpoints: Vec<CatalogEndpoint>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CatalogEndpoint {
    name: String,
    model_id: String,
    model_name: String,
    context_length: u64,
    pricing: CatalogPricing,
    provider_name: String,
    tag: String,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct ErrorDetail {
    code: u16,
    message: &'static str,
}

pub async fn get_models(
    State(catalog): State<Catalog>,
    Extension(profile): Extension<RoutingProfile>,
) -> impl IntoResponse {
    Json(catalog.snapshot(profile).response.as_ref().clone())
}

pub async fn get_model_endpoints(
    State(catalog): State<Catalog>,
    Extension(profile): Extension<RoutingProfile>,
    Path(model): Path<String>,
) -> Response {
    catalog.endpoint_response(profile, &model)
}

pub async fn get_model_endpoints_by_author(
    State(catalog): State<Catalog>,
    Extension(profile): Extension<RoutingProfile>,
    Path((author, slug)): Path<(String, String)>,
) -> Response {
    catalog.endpoint_response(profile, &format!("{author}/{slug}"))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn fixture_catalog() -> Catalog {
        let registry: Registry =
            serde_yaml::from_str(include_str!("../tests/fixtures/catalog_registry.yaml")).unwrap();
        Catalog::from_registry(&registry)
    }

    #[test]
    fn registry_aggregation_matches_live_shape_golden() {
        let catalog = fixture_catalog();
        let actual = serde_json::to_value(
            catalog
                .snapshot(RoutingProfile::Production)
                .response
                .as_ref(),
        )
        .unwrap();
        let expected: Value =
            serde_json::from_str(include_str!("../tests/golden/models_catalog.json")).unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn endpoint_catalog_includes_only_enabled_providers_in_priority_order() {
        let catalog = fixture_catalog();
        let actual = serde_json::to_value(
            &catalog
                .snapshot(RoutingProfile::Production)
                .endpoints_by_slug["anthropic/claude-opus-5-fast"],
        )
        .unwrap();

        assert_eq!(
            actual,
            json!({
                "data": {
                    "id": "anthropic/claude-opus-5-fast",
                    "name": "Claude Opus 5 (Fast)",
                    "endpoints": [
                        {
                            "name": "Balanced Provider: Claude Opus 5 (Fast)",
                            "model_id": "anthropic/claude-opus-5-fast",
                            "model_name": "Claude Opus 5 (Fast)",
                            "context_length": 1000000,
                            "pricing": {
                                "prompt": "0.00001",
                                "completion": "0.00005"
                            },
                            "provider_name": "Balanced Provider",
                            "tag": "balanced"
                        },
                        {
                            "name": "Cheap Input Provider: Claude Opus 5 Fast Alternate",
                            "model_id": "anthropic/claude-opus-5-fast",
                            "model_name": "Claude Opus 5 Fast Alternate",
                            "context_length": 750000,
                            "pricing": {
                                "prompt": "0.000005",
                                "completion": "0.00007"
                            },
                            "provider_name": "Cheap Input Provider",
                            "tag": "cheap-input"
                        }
                    ]
                }
            })
        );
        let serialized = actual.to_string();
        assert!(!serialized.contains("disabled"));
        assert!(!serialized.contains("base_url"));
        assert!(!serialized.contains("secret_env"));
    }

    #[test]
    fn beta_only_providers_are_absent_from_the_production_catalog() {
        let mut registry: Registry =
            serde_yaml::from_str(include_str!("../tests/fixtures/catalog_registry.yaml")).unwrap();
        registry.providers[1].profiles = std::collections::BTreeSet::from([RoutingProfile::Beta]);
        let catalog = Catalog::from_registry(&registry);
        let production = serde_json::to_string(
            catalog
                .snapshot(RoutingProfile::Production)
                .response
                .as_ref(),
        )
        .unwrap();
        let beta = serde_json::to_string(catalog.snapshot(RoutingProfile::Beta).response.as_ref())
            .unwrap();

        assert!(!production.contains("openai/gpt-5-mini"));
        assert!(beta.contains("openai/gpt-5-mini"));
        assert!(!production.contains("Cheap Input Provider"));
    }

    #[test]
    fn endpoint_catalog_uses_public_aliases_without_changing_internal_provider_id() {
        let mut registry: Registry =
            serde_yaml::from_str(include_str!("../tests/fixtures/catalog_registry.yaml")).unwrap();
        registry.providers[0].public_display_name = Some("Seren Inference".to_owned());
        registry.providers[0].public_tag = Some("seren".to_owned());
        registry.validate().unwrap();

        let catalog = Catalog::from_registry(&registry);
        let endpoint = &catalog
            .snapshot(RoutingProfile::Production)
            .endpoints_by_slug["anthropic/claude-opus-5-fast"]
            .data
            .endpoints[0];

        assert_eq!(endpoint.name, "Seren Inference: Claude Opus 5 (Fast)");
        assert_eq!(endpoint.provider_name, "Seren Inference");
        assert_eq!(endpoint.tag, "seren");
        assert_eq!(registry.providers[0].id, "balanced");
    }

    #[test]
    fn checked_modal_route_is_neutral_in_production_and_beta() {
        let registry: Registry =
            serde_yaml::from_str(include_str!("../registry/providers.yaml")).unwrap();
        registry.validate().unwrap();
        let modal = registry
            .providers
            .iter()
            .find(|provider| provider.id == "modal")
            .expect("the checked registry must carry the reviewed Modal route");
        assert_eq!(modal.id, "modal");
        assert!(modal.enabled);
        assert_eq!(
            modal.profiles,
            [RoutingProfile::Production, RoutingProfile::Beta]
                .into_iter()
                .collect()
        );

        let catalog = Catalog::from_registry(&registry);
        let production = &catalog
            .snapshot(RoutingProfile::Production)
            .endpoints_by_slug["moonshotai/kimi-k3"];
        assert_eq!(production.data.endpoints.len(), 2);
        let production_neutral = production
            .data
            .endpoints
            .iter()
            .find(|endpoint| endpoint.provider_name == "Seren Inference")
            .expect("production catalog must publish the neutral direct provider alias");
        assert_eq!(
            production_neutral.name,
            "Seren Inference: MoonshotAI Kimi K3"
        );
        assert_eq!(production_neutral.tag, "seren");
        assert!(
            production
                .data
                .endpoints
                .iter()
                .any(|endpoint| endpoint.provider_name == "OpenRouter"),
            "production catalog must retain the OpenRouter fallback"
        );
        let production_serialized = serde_json::to_string(production)
            .unwrap()
            .to_ascii_lowercase();
        assert!(
            !production_serialized.contains("modal"),
            "the production endpoint catalog must not disclose its internal provider"
        );

        let beta = &catalog.snapshot(RoutingProfile::Beta).endpoints_by_slug["moonshotai/kimi-k3"];
        assert_eq!(beta.data.endpoints.len(), 2);
        let neutral = beta
            .data
            .endpoints
            .iter()
            .find(|endpoint| endpoint.provider_name == "Seren Inference")
            .expect("beta catalog must publish the neutral provider alias");
        assert_eq!(neutral.name, "Seren Inference: MoonshotAI Kimi K3");
        assert_eq!(neutral.tag, "seren");

        let serialized = serde_json::to_string(beta).unwrap().to_ascii_lowercase();
        assert!(
            !serialized.contains("modal"),
            "the beta endpoint catalog must not disclose its internal provider"
        );
        assert_eq!(
            registry
                .providers
                .iter()
                .find(|provider| provider.id == "modal")
                .unwrap()
                .id,
            "modal"
        );
    }
}
