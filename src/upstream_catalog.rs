// ABOUTME: Hydrates fallback-provider coverage from an upstream OpenRouter-shaped model catalog.
// ABOUTME: Keeps explicit registry mappings authoritative and drops models without a usable price.

use std::collections::HashSet;
use std::time::Duration;

use rust_decimal::Decimal;
use serde::Deserialize;
use thiserror::Error;

use crate::registry::{ModelMapping, Registry, RequestConstraints};

const TOKENS_PER_MILLION: u64 = 1_000_000;
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum UpstreamCatalogError {
    #[error("failed to request upstream catalog {url}: {source}")]
    Request { url: String, source: reqwest::Error },
    #[error("upstream catalog {url} returned HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("failed to decode upstream catalog {url}: {source}")]
    Decode { url: String, source: reqwest::Error },
}

#[derive(Debug, Deserialize)]
struct UpstreamCatalogResponse {
    data: Vec<UpstreamModel>,
}

#[derive(Debug, Deserialize)]
struct UpstreamModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    pricing: Option<UpstreamPricing>,
}

#[derive(Debug, Deserialize)]
struct UpstreamPricing {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    completion: Option<String>,
    #[serde(default)]
    input_cache_read: Option<String>,
}

/// Upstream models converted to registry mappings, with the count the upstream
/// advertised so operators can see how many were dropped as unpriceable.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UpstreamCatalog {
    pub mappings: Vec<ModelMapping>,
    pub advertised: usize,
}

/// Outcome of hydrating one provider, for operator-visible startup logging.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HydrationOutcome {
    pub added: usize,
    pub explicit_retained: usize,
}

/// Fetch an OpenRouter-shaped catalog and convert it to registry mappings.
///
/// Models without a non-negative prompt and completion price, a display name,
/// or a context length are dropped: every routable model must carry a Decimal
/// rate so the `usage.cost` money path always has its defined fallback.
pub async fn fetch_catalog(url: &str) -> Result<UpstreamCatalog, UpstreamCatalogError> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|source| UpstreamCatalogError::Request {
            url: url.to_owned(),
            source,
        })?;
    let response =
        client
            .get(url)
            .send()
            .await
            .map_err(|source| UpstreamCatalogError::Request {
                url: url.to_owned(),
                source,
            })?;
    let status = response.status();
    if !status.is_success() {
        return Err(UpstreamCatalogError::Status {
            url: url.to_owned(),
            status: status.as_u16(),
        });
    }
    let catalog = response
        .json::<UpstreamCatalogResponse>()
        .await
        .map_err(|source| UpstreamCatalogError::Decode {
            url: url.to_owned(),
            source,
        })?;

    Ok(mappings_from_catalog(catalog.data))
}

fn mappings_from_catalog(models: Vec<UpstreamModel>) -> UpstreamCatalog {
    let advertised = models.len();
    UpstreamCatalog {
        mappings: models.into_iter().filter_map(mapping_from_model).collect(),
        advertised,
    }
}

fn mapping_from_model(model: UpstreamModel) -> Option<ModelMapping> {
    let pricing = model.pricing?;
    let input_price_per_mtok = price_per_mtok(pricing.prompt.as_deref())?;
    let output_price_per_mtok = price_per_mtok(pricing.completion.as_deref())?;
    let cached_input_price_per_mtok = price_per_mtok(pricing.input_cache_read.as_deref());
    let context_length = model.context_length.filter(|length| *length > 0)?;
    let name = model
        .name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| model.id.clone());

    Some(ModelMapping {
        provider_model_id: model.id.clone(),
        slug: model.id,
        name,
        context_length,
        input_price_per_mtok,
        output_price_per_mtok,
        cached_input_price_per_mtok,
        request_constraints: RequestConstraints::default(),
    })
}

/// Convert an upstream per-token price string to a per-million-token Decimal.
///
/// Negative rates mark upstream meta-routers whose cost is only known after the
/// fact (`openrouter/auto`); they cannot satisfy the registry price contract.
fn price_per_mtok(price_per_token: Option<&str>) -> Option<Decimal> {
    let parsed: Decimal = price_per_token?.trim().parse().ok()?;
    if parsed.is_sign_negative() {
        return None;
    }
    parsed.checked_mul(Decimal::from(TOKENS_PER_MILLION))
}

/// Merge upstream coverage into every provider that declares a `catalog_url`.
///
/// Explicit registry mappings win: a slug already listed on the provider keeps
/// its reviewed price, display name, and request constraints.
pub fn hydrate_provider(
    registry: &mut Registry,
    provider_id: &str,
    upstream: Vec<ModelMapping>,
) -> HydrationOutcome {
    let Some(provider) = registry
        .providers
        .iter_mut()
        .find(|provider| provider.id == provider_id)
    else {
        return HydrationOutcome::default();
    };

    let explicit: HashSet<String> = provider
        .models
        .iter()
        .map(|model| model.slug.clone())
        .collect();
    let explicit_retained = explicit.len();
    let mut added = 0;
    for mapping in upstream {
        if explicit.contains(&mapping.slug) {
            continue;
        }
        provider.models.push(mapping);
        added += 1;
    }

    HydrationOutcome {
        added,
        explicit_retained,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Provider;
    use crate::routing_profile::RoutingProfile;

    fn upstream(id: &str, prompt: &str, completion: &str) -> UpstreamModel {
        UpstreamModel {
            id: id.to_owned(),
            name: Some(format!("Display {id}")),
            context_length: Some(1_000),
            pricing: Some(UpstreamPricing {
                prompt: Some(prompt.to_owned()),
                completion: Some(completion.to_owned()),
                input_cache_read: None,
            }),
        }
    }

    fn provider(id: &str, models: Vec<ModelMapping>) -> Provider {
        Provider {
            id: id.to_owned(),
            display_name: id.to_owned(),
            public_display_name: None,
            public_tag: None,
            base_url: "https://upstream.example/v1".to_owned(),
            secret_env: "SEREN_ROUTER_KEY_UPSTREAM".to_owned(),
            enabled: true,
            priority: 255,
            catalog_url: None,
            profiles: [RoutingProfile::Production].into_iter().collect(),
            models,
        }
    }

    fn explicit_mapping(slug: &str) -> ModelMapping {
        ModelMapping {
            slug: slug.to_owned(),
            name: "Reviewed Name".to_owned(),
            context_length: 42,
            provider_model_id: slug.to_owned(),
            input_price_per_mtok: Decimal::from(7),
            cached_input_price_per_mtok: None,
            output_price_per_mtok: Decimal::from(9),
            request_constraints: RequestConstraints::default(),
        }
    }

    #[test]
    fn per_token_prices_convert_to_exact_per_million_token_decimals() {
        let mapping = mapping_from_model(upstream("vendor/model", "0.00001", "0.00005")).unwrap();
        assert_eq!(mapping.input_price_per_mtok, Decimal::from(10));
        assert_eq!(mapping.output_price_per_mtok, Decimal::from(50));
        assert_eq!(mapping.slug, "vendor/model");
        assert_eq!(mapping.provider_model_id, "vendor/model");
    }

    #[test]
    fn free_models_are_kept_and_variable_priced_meta_routers_are_dropped() {
        assert!(mapping_from_model(upstream("vendor/free", "0", "0")).is_some());
        assert!(mapping_from_model(upstream("openrouter/auto", "-1", "-1")).is_none());
    }

    #[test]
    fn models_without_a_usable_price_or_context_length_are_dropped() {
        let mut missing_pricing = upstream("vendor/model", "0.1", "0.1");
        missing_pricing.pricing = None;
        assert!(mapping_from_model(missing_pricing).is_none());

        let mut zero_context = upstream("vendor/model", "0.1", "0.1");
        zero_context.context_length = Some(0);
        assert!(mapping_from_model(zero_context).is_none());

        let mut unparseable = upstream("vendor/model", "not-a-number", "0.1");
        unparseable.pricing.as_mut().unwrap().prompt = Some("not-a-number".to_owned());
        assert!(mapping_from_model(unparseable).is_none());
    }

    #[test]
    fn explicit_registry_mappings_survive_hydration() {
        let mut registry = Registry {
            providers: vec![provider(
                "openrouter",
                vec![explicit_mapping("vendor/kept")],
            )],
        };

        let outcome = hydrate_provider(
            &mut registry,
            "openrouter",
            mappings_from_catalog(vec![
                upstream("vendor/kept", "0.001", "0.002"),
                upstream("vendor/added", "0.001", "0.002"),
            ])
            .mappings,
        );

        assert_eq!(outcome.added, 1);
        assert_eq!(outcome.explicit_retained, 1);
        let models = &registry.providers[0].models;
        assert_eq!(models.len(), 2);
        let kept = models.iter().find(|m| m.slug == "vendor/kept").unwrap();
        assert_eq!(kept.name, "Reviewed Name");
        assert_eq!(kept.input_price_per_mtok, Decimal::from(7));
        assert_eq!(kept.context_length, 42);
        assert!(models.iter().any(|m| m.slug == "vendor/added"));
    }

    #[test]
    fn hydration_leaves_the_registry_valid() {
        let mut registry = Registry {
            providers: vec![provider(
                "openrouter",
                vec![explicit_mapping("vendor/kept")],
            )],
        };
        hydrate_provider(
            &mut registry,
            "openrouter",
            mappings_from_catalog(vec![upstream("vendor/added", "0.001", "0.002")]).mappings,
        );
        registry.validate().unwrap();
    }
}
