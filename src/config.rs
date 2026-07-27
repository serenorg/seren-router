// ABOUTME: Loads seren-router's sidecar, registry, and Gateway-auth settings.
// ABOUTME: Keeps secret values private and out of formatted configuration errors.

use std::env;
use std::path::{Path, PathBuf};

use reqwest::Url;
use rust_decimal::Decimal;
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::policy::select::{PolicyConfig, PolicyConfigError, ShareTracker, ShareTrackerError};

const DEFAULT_SIDECAR_URL: &str = "http://127.0.0.1:4000";
const DEFAULT_SIDECAR_READINESS_URL: &str = "http://127.0.0.1:19001/healthz/ready";
const DEFAULT_REGISTRY_PATH: &str = "registry/providers.yaml";
const GATEWAY_KEY_ENV: &str = "SEREN_ROUTER_GATEWAY_KEY";
const BETA_GATEWAY_KEY_ENV: &str = "SEREN_ROUTER_BETA_GATEWAY_KEY";
const REGISTRY_PATH_ENV: &str = "SEREN_ROUTER_REGISTRY_PATH";
const SIDECAR_READINESS_URL_ENV: &str = "SEREN_ROUTER_SIDECAR_READINESS_URL";
const SIDECAR_URL_ENV: &str = "SEREN_ROUTER_SIDECAR_URL";
const PRICE_CEILING_ENV: &str = "SEREN_ROUTER_COMBINED_PRICE_CEILING_PER_MTOK";
const HYSTERESIS_ENV: &str = "SEREN_ROUTER_HYSTERESIS_FRACTION";
const MAX_SHARE_ENV: &str = "SEREN_ROUTER_MAX_SHARE";
const SHARE_WINDOW_ENV: &str = "SEREN_ROUTER_SHARE_WINDOW";

pub struct RouterConfig {
    sidecar_url: String,
    sidecar_readiness_url: Url,
    gateway_key: String,
    beta_gateway_key: Option<String>,
    registry_path: PathBuf,
    routing: RoutingConfig,
}

impl RouterConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let gateway_key = required_env(GATEWAY_KEY_ENV)?;
        let beta_gateway_key = optional_present_env(BETA_GATEWAY_KEY_ENV)?;
        let sidecar_url = optional_env(SIDECAR_URL_ENV, DEFAULT_SIDECAR_URL)?;
        let sidecar_readiness_url = parse_http_url(
            SIDECAR_READINESS_URL_ENV,
            &optional_env(SIDECAR_READINESS_URL_ENV, DEFAULT_SIDECAR_READINESS_URL)?,
        )?;
        let registry_path = registry_path_from_env()?;
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

        validate_gateway_keys(&gateway_key, beta_gateway_key.as_deref())?;
        if sidecar_url.trim().is_empty() {
            return Err(ConfigError::Empty {
                name: SIDECAR_URL_ENV,
            });
        }
        Ok(Self {
            sidecar_url: sidecar_url.trim().to_owned(),
            sidecar_readiness_url,
            gateway_key,
            beta_gateway_key,
            registry_path,
            routing,
        })
    }

    pub fn sidecar_url(&self) -> &str {
        &self.sidecar_url
    }

    pub fn sidecar_readiness_url(&self) -> &Url {
        &self.sidecar_readiness_url
    }

    pub fn gateway_key(&self) -> &[u8] {
        self.gateway_key.as_bytes()
    }

    pub fn beta_gateway_key(&self) -> Option<&[u8]> {
        self.beta_gateway_key.as_deref().map(str::as_bytes)
    }

    pub fn registry_path(&self) -> &Path {
        &self.registry_path
    }

    pub fn routing(&self) -> RoutingConfig {
        self.routing
    }
}

pub fn registry_path_from_env() -> Result<PathBuf, ConfigError> {
    let registry_path = optional_env(REGISTRY_PATH_ENV, DEFAULT_REGISTRY_PATH)?;
    if registry_path.trim().is_empty() {
        return Err(ConfigError::Empty {
            name: REGISTRY_PATH_ENV,
        });
    }
    Ok(PathBuf::from(registry_path.trim()))
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

#[derive(Debug, Eq, Error, PartialEq)]
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

fn optional_present_env(name: &'static str) -> Result<Option<String>, ConfigError> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
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

fn parse_http_url(name: &'static str, value: &str) -> Result<Url, ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::Empty { name });
    }
    let url = Url::parse(value.trim()).map_err(|_| ConfigError::Invalid { name })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigError::Invalid { name });
    }
    Ok(url)
}

fn validate_gateway_keys(
    gateway_key: &str,
    beta_gateway_key: Option<&str>,
) -> Result<(), ConfigError> {
    if gateway_key.trim().is_empty() {
        return Err(ConfigError::Empty {
            name: GATEWAY_KEY_ENV,
        });
    }
    if beta_gateway_key.is_some_and(|key| key.trim().is_empty()) {
        return Err(ConfigError::Empty {
            name: BETA_GATEWAY_KEY_ENV,
        });
    }
    if beta_gateway_key
        .is_some_and(|beta_key| bool::from(beta_key.as_bytes().ct_eq(gateway_key.as_bytes())))
    {
        return Err(ConfigError::Invalid {
            name: BETA_GATEWAY_KEY_ENV,
        });
    }
    Ok(())
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

    #[test]
    fn sidecar_readiness_url_requires_plain_http_endpoint_without_credentials() {
        assert!(
            parse_http_url(
                SIDECAR_READINESS_URL_ENV,
                "http://127.0.0.1:19001/healthz/ready"
            )
            .is_ok()
        );
        for invalid in [
            "",
            "not-a-url",
            "file:///tmp/ready",
            "http://user:secret@127.0.0.1/ready",
            "http://127.0.0.1/ready?token=secret",
            "http://127.0.0.1/ready#fragment",
        ] {
            assert!(matches!(
                parse_http_url(SIDECAR_READINESS_URL_ENV, invalid),
                Err(ConfigError::Empty { .. } | ConfigError::Invalid { .. })
            ));
        }
    }

    #[test]
    fn beta_gateway_key_is_optional_but_must_be_distinct_and_nonempty() {
        assert!(validate_gateway_keys("production", None).is_ok());
        assert!(validate_gateway_keys("production", Some("beta")).is_ok());
        assert!(matches!(
            validate_gateway_keys(" \t", None),
            Err(ConfigError::Empty {
                name: GATEWAY_KEY_ENV
            })
        ));
        assert_eq!(
            validate_gateway_keys("production", Some("")),
            Err(ConfigError::Empty {
                name: BETA_GATEWAY_KEY_ENV
            })
        );
        assert!(matches!(
            validate_gateway_keys("production", Some(" \t")),
            Err(ConfigError::Empty {
                name: BETA_GATEWAY_KEY_ENV
            })
        ));
        assert_eq!(
            validate_gateway_keys("same", Some("same")),
            Err(ConfigError::Invalid {
                name: BETA_GATEWAY_KEY_ENV
            })
        );
    }
}
