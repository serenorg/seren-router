// ABOUTME: Loads seren-router's sidecar, registry, and Gateway-auth settings.
// ABOUTME: Keeps secret values private and out of formatted configuration errors.

use std::env;
use std::path::{Path, PathBuf};

use thiserror::Error;

const DEFAULT_SIDECAR_URL: &str = "http://127.0.0.1:4000";
const DEFAULT_REGISTRY_PATH: &str = "registry/providers.yaml";
const GATEWAY_KEY_ENV: &str = "SEREN_ROUTER_GATEWAY_KEY";
const REGISTRY_PATH_ENV: &str = "SEREN_ROUTER_REGISTRY_PATH";
const SIDECAR_URL_ENV: &str = "SEREN_ROUTER_SIDECAR_URL";

pub struct RouterConfig {
    sidecar_url: String,
    gateway_key: String,
    registry_path: PathBuf,
}

impl RouterConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let gateway_key = required_env(GATEWAY_KEY_ENV)?;
        let sidecar_url = optional_env(SIDECAR_URL_ENV, DEFAULT_SIDECAR_URL)?;
        let registry_path = optional_env(REGISTRY_PATH_ENV, DEFAULT_REGISTRY_PATH)?;

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
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{name} is required")]
    Missing { name: &'static str },
    #[error("{name} must not be empty")]
    Empty { name: &'static str },
    #[error("{name} must contain valid Unicode")]
    InvalidUnicode { name: &'static str },
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
