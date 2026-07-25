// ABOUTME: Loads seren-router's sidecar, registry, and Gateway-auth settings.
// ABOUTME: Keeps secret values private and out of formatted configuration errors.

use std::env;
use std::path::{Path, PathBuf};

use rust_decimal::Decimal;
use thiserror::Error;

use crate::policy::select::{PolicyConfig, PolicyConfigError, ShareTracker, ShareTrackerError};

const DEFAULT_SIDECAR_URL: &str = "http://127.0.0.1:4000";
const DEFAULT_REGISTRY_PATH: &str = "registry/providers.yaml";
const GATEWAY_KEY_ENV: &str = "SEREN_ROUTER_GATEWAY_KEY";
const REGISTRY_PATH_ENV: &str = "SEREN_ROUTER_REGISTRY_PATH";
const SIDECAR_URL_ENV: &str = "SEREN_ROUTER_SIDECAR_URL";
const PRICE_CEILING_ENV: &str = "SEREN_ROUTER_COMBINED_PRICE_CEILING_PER_MTOK";
const HYSTERESIS_ENV: &str = "SEREN_ROUTER_HYSTERESIS_FRACTION";
const MAX_SHARE_ENV: &str = "SEREN_ROUTER_MAX_SHARE";
const SHARE_WINDOW_ENV: &str = "SEREN_ROUTER_SHARE_WINDOW";

pub struct RouterConfig {
    sidecar_url: String,
    gateway_key: String,
    registry_path: PathBuf,
    routing: RoutingConfig,
}

impl RouterConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let gateway_key = required_env(GATEWAY_KEY_ENV)?;
        let sidecar_url = optional_env(SIDECAR_URL_ENV, DEFAULT_SIDECAR_URL)?;
        let registry_path = optional_env(REGISTRY_PATH_ENV, DEFAULT_REGISTRY_PATH)?;
        let price_ceiling = required_parsed_env(PRICE_CEILING_ENV)?;
        let hysteresis = required_parsed_env(HYSTERESIS_ENV)?;
        let max_share = required_parsed_env(MAX_SHARE_ENV)?;
        let share_window = required_parsed_env(SHARE_WINDOW_ENV)?;
        let routing = RoutingConfig::new(price_ceiling, hysteresis, max_share, share_window)
            .map_err(|error| ConfigError::Invalid {
                name: match error {
                    RoutingConfigError::Policy(PolicyConfigError::NegativePriceCeiling) => {
                        PRICE_CEILING_ENV
                    }
                    RoutingConfigError::Policy(PolicyConfigError::InvalidHysteresis) => {
                        HYSTERESIS_ENV
                    }
                    RoutingConfigError::Policy(PolicyConfigError::InvalidMaxShare) => MAX_SHARE_ENV,
                    RoutingConfigError::ShareWindow(ShareTrackerError::ZeroCapacity) => {
                        SHARE_WINDOW_ENV
                    }
                },
            })?;

        if gateway_key.is_empty() {
            return Err(ConfigError::Empty {
                name: GATEWAY_KEY_ENV,
            });
        }
        if sidecar_url.trim().is_empty() {
            return Err(ConfigError::Empty {
                name: SIDECAR_URL_ENV,
            });
        }
        if registry_path.trim().is_empty() {
            return Err(ConfigError::Empty {
                name: REGISTRY_PATH_ENV,
            });
        }

        Ok(Self {
            sidecar_url: sidecar_url.trim().to_owned(),
            gateway_key,
            registry_path: PathBuf::from(registry_path.trim()),
            routing,
        })
    }

    pub fn sidecar_url(&self) -> &str {
        &self.sidecar_url
    }

    pub fn gateway_key(&self) -> &[u8] {
        self.gateway_key.as_bytes()
    }

    pub fn registry_path(&self) -> &Path {
        &self.registry_path
    }

    pub fn routing(&self) -> RoutingConfig {
        self.routing
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RoutingConfig {
    policy: PolicyConfig,
    share_window: usize,
}

impl RoutingConfig {
    pub fn new(
        combined_price_ceiling_per_mtok: Decimal,
        hysteresis_fraction: f64,
        max_share: Decimal,
        share_window: usize,
    ) -> Result<Self, RoutingConfigError> {
        let policy = PolicyConfig::new(
            combined_price_ceiling_per_mtok,
            hysteresis_fraction,
            max_share,
        )?;
        ShareTracker::new(share_window)?;

        Ok(Self {
            policy,
            share_window,
        })
    }

    pub fn policy(self) -> PolicyConfig {
        self.policy
    }

    pub fn share_window(self) -> usize {
        self.share_window
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum RoutingConfigError {
    #[error(transparent)]
    Policy(#[from] PolicyConfigError),
    #[error(transparent)]
    ShareWindow(#[from] ShareTrackerError),
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{name} is required")]
    Missing { name: &'static str },
    #[error("{name} must not be empty")]
    Empty { name: &'static str },
    #[error("{name} must contain valid Unicode")]
    InvalidUnicode { name: &'static str },
    #[error("{name} has an invalid value")]
    Invalid { name: &'static str },
}

fn required_env(name: &'static str) -> Result<String, ConfigError> {
    match env::var(name) {
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Err(ConfigError::Missing { name }),
        Err(env::VarError::NotUnicode(_)) => Err(ConfigError::InvalidUnicode { name }),
    }
}

fn optional_env(name: &'static str, default: &'static str) -> Result<String, ConfigError> {
    match env::var(name) {
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Ok(default.to_owned()),
        Err(env::VarError::NotUnicode(_)) => Err(ConfigError::InvalidUnicode { name }),
    }
}

fn required_parsed_env<T>(name: &'static str) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
{
    required_env(name)?
        .trim()
        .parse()
        .map_err(|_| ConfigError::Invalid { name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_config_rejects_every_invalid_policy_boundary() {
        assert!(matches!(
            RoutingConfig::new(Decimal::NEGATIVE_ONE, 0.1, Decimal::ONE, 100),
            Err(RoutingConfigError::Policy(
                PolicyConfigError::NegativePriceCeiling
            ))
        ));
        assert!(matches!(
            RoutingConfig::new(Decimal::ONE, f64::NAN, Decimal::ONE, 100),
            Err(RoutingConfigError::Policy(
                PolicyConfigError::InvalidHysteresis
            ))
        ));
        assert!(matches!(
            RoutingConfig::new(Decimal::ONE, 0.1, Decimal::ZERO, 100),
            Err(RoutingConfigError::Policy(
                PolicyConfigError::InvalidMaxShare
            ))
        ));
        assert!(matches!(
            RoutingConfig::new(Decimal::ONE, 0.1, Decimal::ONE, 0),
            Err(RoutingConfigError::ShareWindow(
                ShareTrackerError::ZeroCapacity
            ))
        ));
    }
}
