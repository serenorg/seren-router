// ABOUTME: Builds exact provider-cost lookups from the reviewed registry.
// ABOUTME: Computes served-provider USD cost without floating-point arithmetic.

use std::collections::HashMap;

use rust_decimal::{Decimal, RoundingStrategy};
use thiserror::Error;

use crate::registry::{PriceSide, Registry, RegistryValidationError};

const TOKENS_PER_MILLION: u64 = 1_000_000;
// Matches the request ledger's NUMERIC(18, 10) USD column.
const COST_SCALE: u32 = 10;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelPrices {
    pub input_price_per_mtok: Decimal,
    pub output_price_per_mtok: Decimal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderPrices {
    pub provider_cost: ModelPrices,
    pub provider_cached_input_price_per_mtok: Option<Decimal>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PriceTable {
    prices_by_provider: HashMap<String, HashMap<String, ProviderPrices>>,
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum PriceTableError {
    #[error(transparent)]
    InvalidRegistry(#[from] RegistryValidationError),
    #[error("duplicate price mapping for provider {provider_id} and model {canonical_slug}")]
    DuplicateMapping {
        provider_id: String,
        canonical_slug: String,
    },
    #[error("negative {price_side} price for provider {provider_id} and model {canonical_slug}")]
    NegativePrice {
        provider_id: String,
        canonical_slug: String,
        price_side: PriceSide,
    },
}

impl PriceTable {
    pub fn from_registry(registry: &Registry) -> Result<Self, PriceTableError> {
        registry.validate()?;
        let enabled_provider_count = registry
            .providers
            .iter()
            .filter(|provider| provider.enabled)
            .count();
        let mut prices_by_provider = HashMap::with_capacity(enabled_provider_count);

        for provider in registry
            .providers
            .iter()
            .filter(|provider| provider.enabled)
        {
            let provider_prices = prices_by_provider
                .entry(provider.id.clone())
                .or_insert_with(|| HashMap::with_capacity(provider.models.len()));

            for model in &provider.models {
                for (price_side, price) in [
                    (PriceSide::Input, Some(model.input_price_per_mtok)),
                    (PriceSide::CachedInput, model.cached_input_price_per_mtok),
                    (PriceSide::Output, Some(model.output_price_per_mtok)),
                ]
                .into_iter()
                .filter_map(|(price_side, price)| price.map(|price| (price_side, price)))
                {
                    if price < Decimal::ZERO {
                        return Err(PriceTableError::NegativePrice {
                            provider_id: provider.id.clone(),
                            canonical_slug: model.slug.clone(),
                            price_side,
                        });
                    }
                }

                let model_prices = ProviderPrices {
                    provider_cost: ModelPrices {
                        input_price_per_mtok: model.input_price_per_mtok,
                        output_price_per_mtok: model.output_price_per_mtok,
                    },
                    provider_cached_input_price_per_mtok: model.cached_input_price_per_mtok,
                };

                if provider_prices
                    .insert(model.slug.clone(), model_prices)
                    .is_some()
                {
                    return Err(PriceTableError::DuplicateMapping {
                        provider_id: provider.id.clone(),
                        canonical_slug: model.slug.clone(),
                    });
                }
            }
        }

        Ok(Self { prices_by_provider })
    }

    pub fn get(&self, provider_id: &str, canonical_slug: &str) -> Option<&ProviderPrices> {
        self.prices_by_provider
            .get(provider_id)
            .and_then(|provider_prices| provider_prices.get(canonical_slug))
    }
}

pub fn cost_usd(prices: &ModelPrices, usage: &Usage) -> Decimal {
    let prompt_cost = Decimal::from(usage.prompt_tokens) * prices.input_price_per_mtok;
    let completion_cost = Decimal::from(usage.completion_tokens) * prices.output_price_per_mtok;

    rounded_cost(prompt_cost + completion_cost)
}

pub fn provider_cost_usd(
    prices: &ProviderPrices,
    usage: &Usage,
    cached_prompt_tokens: Option<u64>,
) -> Option<Decimal> {
    let Some(cached_input_price) = prices.provider_cached_input_price_per_mtok else {
        return Some(cost_usd(&prices.provider_cost, usage));
    };
    let cached_prompt_tokens = cached_prompt_tokens?;
    let uncached_prompt_tokens = usage.prompt_tokens.checked_sub(cached_prompt_tokens)?;
    let prompt_cost = Decimal::from(uncached_prompt_tokens)
        * prices.provider_cost.input_price_per_mtok
        + Decimal::from(cached_prompt_tokens) * cached_input_price;
    let completion_cost =
        Decimal::from(usage.completion_tokens) * prices.provider_cost.output_price_per_mtok;

    Some(rounded_cost(prompt_cost + completion_cost))
}

pub fn normalize_cost_usd(cost: Decimal) -> Option<Decimal> {
    if cost < Decimal::ZERO {
        return None;
    }

    let mut normalized =
        cost.round_dp_with_strategy(COST_SCALE, RoundingStrategy::MidpointAwayFromZero);
    normalized.rescale(COST_SCALE);
    Some(normalized)
}

fn rounded_cost(cost_per_million: Decimal) -> Decimal {
    normalize_cost_usd(cost_per_million / Decimal::from(TOKENS_PER_MILLION))
        .expect("validated non-negative prices and usage produce a non-negative cost")
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;
    use crate::registry::Registry;

    const PRICE_REGISTRY: &str = r#"
providers:
  - id: enabled-provider
    display_name: Enabled Provider
    base_url: https://enabled.example/v1
    secret_env: ENABLED_KEY
    enabled: true
    priority: 0
    models:
      - slug: canonical/model
        name: Canonical Model
        context_length: 131072
        provider_model_id: provider/model
        input_price_per_mtok: "0.1234500"
        output_price_per_mtok: "0.80"
  - id: disabled-provider
    display_name: Disabled Provider
    base_url: https://disabled.example/v1
    secret_env: DISABLED_KEY
    enabled: false
    priority: 1
    models:
      - slug: canonical/model
        name: Canonical Model
        context_length: 131072
        provider_model_id: provider/model
        input_price_per_mtok: "9.99"
        output_price_per_mtok: "9.99"
"#;

    #[test]
    fn table_indexes_enabled_provider_and_model_without_inventing_prices() {
        let registry: Registry = serde_yaml::from_str(PRICE_REGISTRY).unwrap();
        let table = PriceTable::from_registry(&registry).unwrap();

        assert_eq!(
            table.get("enabled-provider", "canonical/model"),
            Some(&ProviderPrices {
                provider_cost: ModelPrices {
                    input_price_per_mtok: "0.1234500".parse().unwrap(),
                    output_price_per_mtok: "0.80".parse().unwrap(),
                },
                provider_cached_input_price_per_mtok: None,
            })
        );
        assert_eq!(table.get("disabled-provider", "canonical/model"), None);
        assert_eq!(table.get("enabled-provider", "unknown/model"), None);
    }

    #[test]
    fn duplicate_enabled_provider_model_price_is_rejected() {
        let mut registry: Registry = serde_yaml::from_str(PRICE_REGISTRY).unwrap();
        let duplicate = registry.providers[0].models[0].clone();
        registry.providers[0].models.push(duplicate);

        assert_eq!(
            PriceTable::from_registry(&registry),
            Err(PriceTableError::DuplicateMapping {
                provider_id: "enabled-provider".to_owned(),
                canonical_slug: "canonical/model".to_owned(),
            })
        );
    }

    #[test]
    fn negative_provider_prices_are_rejected() {
        for (price_side, input_price, cached_input_price, output_price) in [
            (PriceSide::Input, "-0.01", None, "0"),
            (PriceSide::CachedInput, "0", Some("-0.01"), "0"),
            (PriceSide::Output, "0", None, "-0.01"),
        ] {
            let mut registry: Registry = serde_yaml::from_str(PRICE_REGISTRY).unwrap();
            registry.providers[0].models[0].input_price_per_mtok = input_price.parse().unwrap();
            registry.providers[0].models[0].cached_input_price_per_mtok =
                cached_input_price.map(|price| price.parse().unwrap());
            registry.providers[0].models[0].output_price_per_mtok = output_price.parse().unwrap();

            assert_eq!(
                PriceTable::from_registry(&registry),
                Err(PriceTableError::NegativePrice {
                    provider_id: "enabled-provider".to_owned(),
                    canonical_slug: "canonical/model".to_owned(),
                    price_side,
                })
            );
        }
    }

    #[test]
    fn cost_is_exact_decimal_usd() {
        struct Case {
            name: &'static str,
            input_price: &'static str,
            output_price: &'static str,
            usage: Usage,
            expected: &'static str,
        }

        let cases = [
            Case {
                name: "zero tokens",
                input_price: "1.25",
                output_price: "2.50",
                usage: Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                },
                expected: "0.0000000000",
            },
            Case {
                name: "exact million boundary",
                input_price: "1.25",
                output_price: "2.50",
                usage: Usage {
                    prompt_tokens: 1_000_000,
                    completion_tokens: 1_000_000,
                },
                expected: "3.7500000000",
            },
            Case {
                name: "significant trailing-zero precision",
                input_price: "123.4500",
                output_price: "0",
                usage: Usage {
                    prompt_tokens: 1,
                    completion_tokens: 0,
                },
                expected: "0.0001234500",
            },
            Case {
                name: "halfway value rounds away from zero at ledger scale",
                input_price: "0.00005",
                output_price: "0",
                usage: Usage {
                    prompt_tokens: 1,
                    completion_tokens: 0,
                },
                expected: "0.0000000001",
            },
            Case {
                // 1,234 × $0.40 + 567 × $0.80 = $947.20 per million tokens.
                name: "mixed token hand calculation",
                input_price: "0.40",
                output_price: "0.80",
                usage: Usage {
                    prompt_tokens: 1_234,
                    completion_tokens: 567,
                },
                expected: "0.0009472000",
            },
        ];

        for case in cases {
            let prices = ModelPrices {
                input_price_per_mtok: case.input_price.parse::<Decimal>().unwrap(),
                output_price_per_mtok: case.output_price.parse::<Decimal>().unwrap(),
            };

            assert_eq!(
                cost_usd(&prices, &case.usage).to_string(),
                case.expected,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn cached_prompt_provider_cost_is_exact_and_requires_valid_usage() {
        let prices = ProviderPrices {
            provider_cost: ModelPrices {
                input_price_per_mtok: "3.00".parse().unwrap(),
                output_price_per_mtok: "15.00".parse().unwrap(),
            },
            provider_cached_input_price_per_mtok: Some("0.30".parse().unwrap()),
        };
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 10,
        };

        // (60 × $3.00 + 40 × $0.30 + 10 × $15.00) / 1,000,000.
        assert_eq!(
            provider_cost_usd(&prices, &usage, Some(40))
                .unwrap()
                .to_string(),
            "0.0003420000"
        );
        assert_eq!(provider_cost_usd(&prices, &usage, None), None);
        assert_eq!(provider_cost_usd(&prices, &usage, Some(101)), None);
    }

    #[test]
    fn provider_reported_cost_is_normalized_without_floating_point() {
        assert_eq!(
            normalize_cost_usd("0.00034092".parse().unwrap())
                .unwrap()
                .to_string(),
            "0.0003409200"
        );
        assert_eq!(
            normalize_cost_usd("-0.01".parse().unwrap()),
            None,
            "negative provider-reported cost must fail closed"
        );
    }
}
