// ABOUTME: Builds the OpenRouter-shaped model catalog from enabled registry mappings.
// ABOUTME: Serves one deterministic entry per canonical slug with exact per-token prices.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::registry::{ModelMapping, Registry};

const TOKENS_PER_MILLION: u64 = 1_000_000;

#[derive(Clone, Debug)]
pub struct Catalog {
    response: Arc<ModelsResponse>,
}

impl Catalog {
    pub fn from_registry(registry: &Registry) -> Self {
        let mut cheapest_by_slug = BTreeMap::<String, CatalogCandidate<'_>>::new();

        for provider in registry
            .providers
            .iter()
            .filter(|provider| provider.enabled)
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
            }
        }

        let data: Vec<_> = cheapest_by_slug
            .into_values()
            .map(CatalogCandidate::into_model)
            .collect();
        let total_count = data.len();

        Self {
            response: Arc::new(ModelsResponse {
                data,
                links: ModelsLinks { next: None },
                total_count,
            }),
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

pub async fn get_models(State(catalog): State<Catalog>) -> impl IntoResponse {
    Json(catalog.response.as_ref().clone())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn registry_aggregation_matches_live_shape_golden() {
        let registry: Registry =
            serde_yaml::from_str(include_str!("../tests/fixtures/catalog_registry.yaml")).unwrap();
        let catalog = Catalog::from_registry(&registry);
        let actual = serde_json::to_value(catalog.response.as_ref()).unwrap();
        let expected: Value =
            serde_json::from_str(include_str!("../tests/golden/models_catalog.json")).unwrap();

        assert_eq!(actual, expected);
    }
}
