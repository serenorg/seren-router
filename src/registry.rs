// ABOUTME: Defines the declarative inference-provider and model registry.
// ABOUTME: Validates reviewable provider metadata before sidecar config compilation.

use std::collections::HashSet;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub providers: Vec<Provider>,
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
    pub models: Vec<ModelMapping>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMapping {
    pub slug: String,
    pub provider_model_id: String,
    pub input_price_per_mtok: Decimal,
    pub output_price_per_mtok: Decimal,
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum RegistryValidationError {
    #[error("duplicate provider id: {0}")]
    DuplicateProviderId(String),
}

impl Registry {
    pub fn validate(&self) -> Result<(), RegistryValidationError> {
        let mut provider_ids = HashSet::with_capacity(self.providers.len());

        for provider in &self.providers {
            if !provider_ids.insert(provider.id.as_str()) {
                return Err(RegistryValidationError::DuplicateProviderId(
                    provider.id.clone(),
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_PROVIDER_YAML: &str = r#"
providers:
  - id: openrouter
    display_name: OpenRouter
    base_url: https://openrouter.ai/api/v1
    secret_env: SEREN_ROUTER_KEY_OPENROUTER
    enabled: true
    priority: 255
    models:
      - slug: meta-llama/llama-3.3-70b-instruct
        provider_model_id: meta-llama/llama-3.3-70b-instruct
        input_price_per_mtok: "0.40"
        output_price_per_mtok: "0.40"
  - id: fireworks
    display_name: Fireworks AI
    base_url: https://api.fireworks.ai/inference/v1
    secret_env: SEREN_ROUTER_KEY_FIREWORKS
    models:
      - slug: meta-llama/llama-3.3-70b-instruct
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
}
