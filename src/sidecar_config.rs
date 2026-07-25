// ABOUTME: Compiles the validated provider registry into agentgateway YAML.
// ABOUTME: Makes health eviction mandatory for every generated provider route.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use serde::Serialize;
use thiserror::Error;

use crate::attribution::SERVED_PROVIDER_HEADER;
use crate::registry::{ModelMapping, Provider, Registry, RegistryValidationError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SidecarConfigOptions {
    pub llm_port: u16,
    pub admin_addr: Option<SocketAddr>,
    pub stats_addr: Option<SocketAddr>,
    pub readiness_addr: SocketAddr,
}

impl Default for SidecarConfigOptions {
    fn default() -> Self {
        Self {
            llm_port: 4000,
            admin_addr: None,
            stats_addr: None,
            readiness_addr: SocketAddr::from(([127, 0, 0, 1], 19001)),
        }
    }
}

#[derive(Debug, Error)]
pub enum SidecarConfigError {
    #[error(transparent)]
    Registry(#[from] RegistryValidationError),
    #[error("failed to serialize agentgateway config: {0}")]
    Serialize(#[from] serde_yaml::Error),
}

pub fn compile(
    registry: &Registry,
    options: SidecarConfigOptions,
) -> Result<Vec<u8>, SidecarConfigError> {
    let config = build_config(registry, options)?;
    Ok(serde_yaml::to_string(&config)?.into_bytes())
}

fn build_config(
    registry: &Registry,
    options: SidecarConfigOptions,
) -> Result<AgentGatewayConfig, RegistryValidationError> {
    registry.validate()?;

    let mut models = Vec::new();
    let mut targets_by_slug: BTreeMap<String, Vec<VirtualTarget>> = BTreeMap::new();

    for provider in registry
        .providers
        .iter()
        .filter(|provider| provider.enabled)
    {
        for mapping in &provider.models {
            let route = ModelRoute::new(provider, mapping);
            targets_by_slug
                .entry(mapping.slug.clone())
                .or_default()
                .push(VirtualTarget {
                    model: route.name.clone(),
                    priority: provider.priority,
                });
            models.push(route);
        }
    }

    let virtual_models = targets_by_slug
        .into_iter()
        .filter_map(|(slug, mut targets)| {
            if targets.len() < 2 {
                return None;
            }
            targets.sort_by_key(|target| target.priority);
            Some(VirtualModel {
                name: slug,
                routing: VirtualRouting {
                    failover: Failover { targets },
                },
            })
        })
        .collect();

    Ok(AgentGatewayConfig {
        config: RuntimeConfig {
            admin_addr: options.admin_addr,
            stats_addr: options.stats_addr,
            readiness_addr: options.readiness_addr,
        },
        llm: LlmConfig {
            port: options.llm_port,
            models,
            virtual_models,
        },
    })
}

#[derive(Debug, Serialize)]
struct AgentGatewayConfig {
    config: RuntimeConfig,
    llm: LlmConfig,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    admin_addr: Option<SocketAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats_addr: Option<SocketAddr>,
    readiness_addr: SocketAddr,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LlmConfig {
    port: u16,
    models: Vec<ModelRoute>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    virtual_models: Vec<VirtualModel>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelRoute {
    name: String,
    provider: &'static str,
    params: ModelParams,
    response_headers: HeaderModifier,
    health: Health,
}

impl ModelRoute {
    fn new(provider: &Provider, mapping: &ModelMapping) -> Self {
        Self {
            name: format!("{}/{}", provider.id, mapping.slug),
            provider: "openAI",
            params: ModelParams {
                base_url: provider.base_url.clone(),
                model: mapping.provider_model_id.clone(),
                api_key: format!("${}", provider.secret_env),
            },
            response_headers: HeaderModifier {
                set: BTreeMap::from([(SERVED_PROVIDER_HEADER.to_owned(), provider.id.clone())]),
            },
            health: Health {
                eviction: Eviction {
                    consecutive_failures: 1,
                    duration: "60s",
                },
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct HeaderModifier {
    set: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelParams {
    base_url: String,
    model: String,
    api_key: String,
}

#[derive(Debug, Serialize)]
struct Health {
    eviction: Eviction,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Eviction {
    consecutive_failures: u32,
    duration: &'static str,
}

#[derive(Debug, Serialize)]
struct VirtualModel {
    name: String,
    routing: VirtualRouting,
}

#[derive(Debug, Serialize)]
struct VirtualRouting {
    failover: Failover,
}

#[derive(Debug, Serialize)]
struct Failover {
    targets: Vec<VirtualTarget>,
}

#[derive(Debug, Serialize)]
struct VirtualTarget {
    model: String,
    priority: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_registry() -> Registry {
        serde_yaml::from_str(include_str!(
            "../tests/fixtures/sidecar_config_registry.yaml"
        ))
        .unwrap()
    }

    #[test]
    fn fixture_matches_golden_yaml() {
        let yaml = compile(&fixture_registry(), SidecarConfigOptions::default()).unwrap();

        assert_eq!(
            std::str::from_utf8(&yaml).unwrap(),
            include_str!("../tests/golden/sidecar_config_basic.yaml")
        );
    }

    #[test]
    fn management_addresses_can_be_isolated_for_functional_runs() {
        let options = SidecarConfigOptions {
            llm_port: 41001,
            admin_addr: Some(SocketAddr::from(([127, 0, 0, 1], 41002))),
            stats_addr: Some(SocketAddr::from(([127, 0, 0, 1], 41003))),
            readiness_addr: SocketAddr::from(([127, 0, 0, 1], 41004)),
        };
        let yaml = compile(&fixture_registry(), options).unwrap();
        let config: serde_yaml::Value = serde_yaml::from_slice(&yaml).unwrap();

        assert_eq!(config["llm"]["port"], 41001);
        assert_eq!(config["config"]["adminAddr"], "127.0.0.1:41002");
        assert_eq!(config["config"]["statsAddr"], "127.0.0.1:41003");
        assert_eq!(config["config"]["readinessAddr"], "127.0.0.1:41004");
    }

    #[test]
    fn failover_targets_follow_provider_priority() {
        let config = build_config(&fixture_registry(), SidecarConfigOptions::default()).unwrap();
        let priorities: Vec<_> = config.llm.virtual_models[0]
            .routing
            .failover
            .targets
            .iter()
            .map(|target| target.priority)
            .collect();

        assert_eq!(priorities, [5, 20]);
    }

    #[test]
    fn every_route_has_mandatory_eviction() {
        let config = build_config(&fixture_registry(), SidecarConfigOptions::default()).unwrap();

        assert!(config.llm.models.iter().all(|route| {
            route.health.eviction.consecutive_failures == 1
                && route.health.eviction.duration == "60s"
        }));
    }

    #[test]
    fn api_keys_remain_environment_references() {
        let config = build_config(&fixture_registry(), SidecarConfigOptions::default()).unwrap();
        let api_keys: Vec<_> = config
            .llm
            .models
            .iter()
            .map(|route| route.params.api_key.as_str())
            .collect();

        assert_eq!(
            api_keys,
            [
                "$SEREN_ROUTER_KEY_SLOW",
                "$SEREN_ROUTER_KEY_SLOW",
                "$SEREN_ROUTER_KEY_FAST"
            ]
        );
    }
}
