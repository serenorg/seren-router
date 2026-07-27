// ABOUTME: Compiles the validated provider registry into agentgateway YAML.
// ABOUTME: Makes health eviction mandatory for every generated provider route.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use serde::Serialize;
use thiserror::Error;

use crate::attribution::SERVED_PROVIDER_HEADER;
use crate::registry::{ModelMapping, Provider, Registry, RegistryValidationError};
use crate::routing_profile::RoutingProfile;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SidecarConfigOptions {
    pub llm_port: u16,
    pub admin_addr: Option<SocketAddr>,
    pub stats_addr: Option<SocketAddr>,
    pub readiness_addr: SocketAddr,
    pub enable_ipv6: bool,
}

impl Default for SidecarConfigOptions {
    fn default() -> Self {
        Self {
            llm_port: 4000,
            admin_addr: None,
            stats_addr: None,
            readiness_addr: SocketAddr::from(([127, 0, 0, 1], 19001)),
            enable_ipv6: false,
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
    let mut targets_by_profile_slug: BTreeMap<(RoutingProfile, String), Vec<VirtualTarget>> =
        BTreeMap::new();

    for provider in registry
        .providers
        .iter()
        .filter(|provider| provider.enabled)
    {
        for mapping in &provider.models {
            let route = ModelRoute::new(provider, mapping);
            for profile in provider.profiles.iter().copied() {
                targets_by_profile_slug
                    .entry((profile, mapping.slug.clone()))
                    .or_default()
                    .push(VirtualTarget {
                        model: route.name.clone(),
                        priority: provider.priority,
                    });
            }
            models.push(route);
        }
    }

    let virtual_models = targets_by_profile_slug
        .into_iter()
        .map(|((profile, slug), mut targets)| {
            targets.sort_by_key(|target| target.priority);
            VirtualModel {
                name: profile.sidecar_alias(&slug),
                routing: VirtualRouting {
                    failover: Failover { targets },
                },
            }
        })
        .collect();

    Ok(AgentGatewayConfig {
        config: RuntimeConfig {
            admin_addr: options.admin_addr,
            stats_addr: options.stats_addr,
            readiness_addr: options.readiness_addr,
            enable_ipv6: options.enable_ipv6,
        },
        llm: LlmConfig {
            port: options.llm_port,
            models,
            virtual_models,
        },
        policies: vec![TargetedRoutePolicy::llm_retry()],
    })
}

#[derive(Debug, Serialize)]
struct AgentGatewayConfig {
    config: RuntimeConfig,
    llm: LlmConfig,
    policies: Vec<TargetedRoutePolicy>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    admin_addr: Option<SocketAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats_addr: Option<SocketAddr>,
    readiness_addr: SocketAddr,
    enable_ipv6: bool,
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
    passthrough: &'static str,
    overrides: ModelOverrides,
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
            passthrough: "detect",
            overrides: ModelOverrides {
                model: mapping.provider_model_id.clone(),
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
struct ModelOverrides {
    model: String,
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

#[derive(Debug, Serialize)]
struct TargetedRoutePolicy {
    name: ResourceName,
    target: PolicyTarget,
    policy: RoutePolicy,
}

impl TargetedRoutePolicy {
    fn llm_retry() -> Self {
        Self {
            name: ResourceName {
                name: "llm-retry",
                namespace: "internal",
            },
            target: PolicyTarget {
                route: ResourceName {
                    name: "llm:request",
                    namespace: "internal",
                },
            },
            policy: RoutePolicy {
                retry: RetryPolicy {
                    attempts: 2,
                    codes: [429],
                    condition: "response.code >= 500 && response.code < 600",
                },
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct ResourceName {
    name: &'static str,
    namespace: &'static str,
}

#[derive(Debug, Serialize)]
struct PolicyTarget {
    route: ResourceName,
}

#[derive(Debug, Serialize)]
struct RoutePolicy {
    retry: RetryPolicy,
}

#[derive(Debug, Serialize)]
struct RetryPolicy {
    attempts: u8,
    codes: [u16; 1],
    condition: &'static str,
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
            enable_ipv6: false,
        };
        let yaml = compile(&fixture_registry(), options).unwrap();
        let config: serde_yaml::Value = serde_yaml::from_slice(&yaml).unwrap();

        assert_eq!(config["llm"]["port"], 41001);
        assert_eq!(config["config"]["adminAddr"], "127.0.0.1:41002");
        assert_eq!(config["config"]["statsAddr"], "127.0.0.1:41003");
        assert_eq!(config["config"]["readinessAddr"], "127.0.0.1:41004");
        assert_eq!(config["config"]["enableIpv6"], false);
    }

    #[test]
    fn ipv6_resolution_is_disabled_by_default() {
        let config = build_config(&fixture_registry(), SidecarConfigOptions::default()).unwrap();

        assert!(!config.config.enable_ipv6);
    }

    #[test]
    fn failover_targets_follow_provider_priority() {
        let config = build_config(&fixture_registry(), SidecarConfigOptions::default()).unwrap();
        let shared = config
            .llm
            .virtual_models
            .iter()
            .find(|model| model.name == "seren-profile-production/acme/shared")
            .unwrap();
        let priorities: Vec<_> = shared
            .routing
            .failover
            .targets
            .iter()
            .map(|target| target.priority)
            .collect();

        assert_eq!(priorities, [5, 20]);
    }

    #[test]
    fn profile_aliases_never_include_providers_from_another_profile() {
        let mut registry = fixture_registry();
        registry.providers[0].profiles =
            std::collections::BTreeSet::from([RoutingProfile::Production]);
        registry.providers[1].profiles = std::collections::BTreeSet::from([RoutingProfile::Beta]);
        let config = build_config(&registry, SidecarConfigOptions::default()).unwrap();

        let production = config
            .llm
            .virtual_models
            .iter()
            .find(|model| model.name == "seren-profile-production/acme/shared")
            .unwrap();
        let beta = config
            .llm
            .virtual_models
            .iter()
            .find(|model| model.name == "seren-profile-beta/acme/shared")
            .unwrap();

        assert_eq!(production.routing.failover.targets.len(), 1);
        assert_eq!(
            production.routing.failover.targets[0].model,
            "slow/acme/shared"
        );
        assert_eq!(beta.routing.failover.targets.len(), 1);
        assert_eq!(beta.routing.failover.targets[0].model, "fast/acme/shared");
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
    fn every_route_uses_detect_passthrough_with_a_model_override() {
        let config = build_config(&fixture_registry(), SidecarConfigOptions::default()).unwrap();

        assert!(config.llm.models.iter().all(|route| {
            route.passthrough == "detect" && route.overrides.model == route.params.model
        }));
    }

    #[test]
    fn retry_policy_targets_the_generated_llm_route() {
        let config = build_config(&fixture_registry(), SidecarConfigOptions::default()).unwrap();
        let retry = &config.policies[0];

        assert_eq!(retry.name.name, "llm-retry");
        assert_eq!(retry.name.namespace, "internal");
        assert_eq!(retry.target.route.name, "llm:request");
        assert_eq!(retry.target.route.namespace, "internal");
        assert_eq!(retry.policy.retry.attempts, 2);
        assert_eq!(retry.policy.retry.codes, [429]);
        assert_eq!(
            retry.policy.retry.condition,
            "response.code >= 500 && response.code < 600"
        );
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
