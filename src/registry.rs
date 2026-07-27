// ABOUTME: Defines providers, their costs, and route-independent customer sell prices.
// ABOUTME: Validates reviewable registry metadata before sidecar config compilation.

use std::collections::{BTreeSet, HashSet};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::routing_profile::RoutingProfile;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub sell_prices: Vec<SellPrice>,
    pub providers: Vec<Provider>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SellPrice {
    pub slug: String,
    pub input_price_per_mtok: Decimal,
    pub output_price_per_mtok: Decimal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    pub secret_env: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub priority: u8,
    #[serde(default = "default_profiles")]
    pub profiles: BTreeSet<RoutingProfile>,
    pub models: Vec<ModelMapping>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMapping {
    pub slug: String,
    pub name: String,
    pub context_length: u64,
    pub provider_model_id: String,
    pub input_price_per_mtok: Decimal,
    pub output_price_per_mtok: Decimal,
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum RegistryValidationError {
    #[error("duplicate sell price for model {0}")]
    DuplicateSellPrice(String),
    #[error("negative {price_side} sell price for model {slug}")]
    NegativeSellPrice { slug: String, price_side: PriceSide },
    #[error("model {slug} for provider {provider_id} has no reviewed sell price")]
    MissingSellPrice { provider_id: String, slug: String },
    #[error("duplicate provider id: {0}")]
    DuplicateProviderId(String),
    #[error("provider {0} must allow at least one routing profile")]
    EmptyProviderProfiles(String),
    #[error("model {slug} for provider {provider_id} has an empty display name")]
    EmptyModelName { provider_id: String, slug: String },
    #[error("model {slug} for provider {provider_id} has a zero context length")]
    ZeroModelContextLength { provider_id: String, slug: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PriceSide {
    Input,
    Output,
}

impl std::fmt::Display for PriceSide {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input => formatter.write_str("input"),
            Self::Output => formatter.write_str("output"),
        }
    }
}

impl Registry {
    pub fn validate(&self) -> Result<(), RegistryValidationError> {
        let mut sell_price_slugs = HashSet::with_capacity(self.sell_prices.len());
        for sell_price in &self.sell_prices {
            if !sell_price_slugs.insert(sell_price.slug.as_str()) {
                return Err(RegistryValidationError::DuplicateSellPrice(
                    sell_price.slug.clone(),
                ));
            }
            for (price_side, price) in [
                (PriceSide::Input, sell_price.input_price_per_mtok),
                (PriceSide::Output, sell_price.output_price_per_mtok),
            ] {
                if price < Decimal::ZERO {
                    return Err(RegistryValidationError::NegativeSellPrice {
                        slug: sell_price.slug.clone(),
                        price_side,
                    });
                }
            }
        }

        let mut provider_ids = HashSet::with_capacity(self.providers.len());

        for provider in &self.providers {
            if !provider_ids.insert(provider.id.as_str()) {
                return Err(RegistryValidationError::DuplicateProviderId(
                    provider.id.clone(),
                ));
            }
            if provider.profiles.is_empty() {
                return Err(RegistryValidationError::EmptyProviderProfiles(
                    provider.id.clone(),
                ));
            }

            for mapping in &provider.models {
                if !sell_price_slugs.contains(mapping.slug.as_str()) {
                    return Err(RegistryValidationError::MissingSellPrice {
                        provider_id: provider.id.clone(),
                        slug: mapping.slug.clone(),
                    });
                }
                if mapping.name.trim().is_empty() {
                    return Err(RegistryValidationError::EmptyModelName {
                        provider_id: provider.id.clone(),
                        slug: mapping.slug.clone(),
                    });
                }
                if mapping.context_length == 0 {
                    return Err(RegistryValidationError::ZeroModelContextLength {
                        provider_id: provider.id.clone(),
                        slug: mapping.slug.clone(),
                    });
                }
            }
        }

        Ok(())
    }

    pub fn sell_price(&self, slug: &str) -> Option<&SellPrice> {
        self.sell_prices
            .iter()
            .find(|sell_price| sell_price.slug == slug)
    }
}

impl Provider {
    pub fn supports(&self, profile: RoutingProfile) -> bool {
        self.profiles.contains(&profile)
    }
}

fn default_profiles() -> BTreeSet<RoutingProfile> {
    BTreeSet::from([RoutingProfile::Production])
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_PROVIDER_YAML: &str = r#"
sell_prices:
  - slug: meta-llama/llama-3.3-70b-instruct
    input_price_per_mtok: "1.00"
    output_price_per_mtok: "2.00"
providers:
  - id: openrouter
    display_name: OpenRouter
    base_url: https://openrouter.ai/api/v1
    secret_env: SEREN_ROUTER_KEY_OPENROUTER
    enabled: true
    priority: 255
    models:
      - slug: meta-llama/llama-3.3-70b-instruct
        name: Llama 3.3 70B Instruct
        context_length: 131072
        provider_model_id: meta-llama/llama-3.3-70b-instruct
        input_price_per_mtok: "0.40"
        output_price_per_mtok: "0.40"
  - id: fireworks
    display_name: Fireworks AI
    base_url: https://api.fireworks.ai/inference/v1
    secret_env: SEREN_ROUTER_KEY_FIREWORKS
    models:
      - slug: meta-llama/llama-3.3-70b-instruct
        name: Llama 3.3 70B Instruct
        context_length: 131072
        provider_model_id: accounts/fireworks/models/llama-v3p3-70b-instruct
        input_price_per_mtok: "0.90"
        output_price_per_mtok: "0.90"
"#;

    #[test]
    fn two_provider_yaml_round_trips() {
        let registry: Registry = serde_yaml::from_str(TWO_PROVIDER_YAML).unwrap();
        registry.validate().unwrap();

        let serialized = serde_yaml::to_string(&registry).unwrap();
        let reparsed: Registry = serde_yaml::from_str(&serialized).unwrap();

        assert_eq!(reparsed, registry);
        assert!(!registry.providers[1].enabled);
        assert_eq!(registry.providers[1].priority, 0);
        assert_eq!(
            registry.providers[1].profiles,
            BTreeSet::from([RoutingProfile::Production])
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let yaml = TWO_PROVIDER_YAML.replace(
            "        output_price_per_mtok: \"0.90\"",
            "        output_price_per_mtok: \"0.90\"\n        leaked_field: true",
        );

        let error = serde_yaml::from_str::<Registry>(&yaml).unwrap_err();

        assert!(error.to_string().contains("unknown field `leaked_field`"));
    }

    #[test]
    fn duplicate_provider_ids_are_rejected() {
        let mut registry: Registry = serde_yaml::from_str(TWO_PROVIDER_YAML).unwrap();
        registry.providers[1].id = registry.providers[0].id.clone();

        assert_eq!(
            registry.validate(),
            Err(RegistryValidationError::DuplicateProviderId(
                "openrouter".to_owned()
            ))
        );
    }

    #[test]
    fn duplicate_negative_and_missing_sell_prices_are_rejected() {
        let mut duplicate: Registry = serde_yaml::from_str(TWO_PROVIDER_YAML).unwrap();
        duplicate.sell_prices.push(duplicate.sell_prices[0].clone());
        assert_eq!(
            duplicate.validate(),
            Err(RegistryValidationError::DuplicateSellPrice(
                "meta-llama/llama-3.3-70b-instruct".to_owned()
            ))
        );

        let mut negative: Registry = serde_yaml::from_str(TWO_PROVIDER_YAML).unwrap();
        negative.sell_prices[0].output_price_per_mtok = "-0.01".parse().unwrap();
        assert_eq!(
            negative.validate(),
            Err(RegistryValidationError::NegativeSellPrice {
                slug: "meta-llama/llama-3.3-70b-instruct".to_owned(),
                price_side: PriceSide::Output,
            })
        );

        let mut missing: Registry = serde_yaml::from_str(TWO_PROVIDER_YAML).unwrap();
        missing.sell_prices.clear();
        assert_eq!(
            missing.validate(),
            Err(RegistryValidationError::MissingSellPrice {
                provider_id: "openrouter".to_owned(),
                slug: "meta-llama/llama-3.3-70b-instruct".to_owned(),
            })
        );
    }

    #[test]
    fn blank_model_names_are_rejected() {
        let mut registry: Registry = serde_yaml::from_str(TWO_PROVIDER_YAML).unwrap();
        registry.providers[0].models[0].name = " \t".to_owned();

        assert_eq!(
            registry.validate(),
            Err(RegistryValidationError::EmptyModelName {
                provider_id: "openrouter".to_owned(),
                slug: "meta-llama/llama-3.3-70b-instruct".to_owned(),
            })
        );
    }

    #[test]
    fn zero_model_context_lengths_are_rejected() {
        let mut registry: Registry = serde_yaml::from_str(TWO_PROVIDER_YAML).unwrap();
        registry.providers[0].models[0].context_length = 0;

        assert_eq!(
            registry.validate(),
            Err(RegistryValidationError::ZeroModelContextLength {
                provider_id: "openrouter".to_owned(),
                slug: "meta-llama/llama-3.3-70b-instruct".to_owned(),
            })
        );
    }

    #[test]
    fn empty_provider_profiles_are_rejected() {
        let mut registry: Registry = serde_yaml::from_str(TWO_PROVIDER_YAML).unwrap();
        registry.providers[0].profiles.clear();

        assert_eq!(
            registry.validate(),
            Err(RegistryValidationError::EmptyProviderProfiles(
                "openrouter".to_owned()
            ))
        );
    }
}
