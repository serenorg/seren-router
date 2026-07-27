// ABOUTME: Defines credential-bound routing profiles for production and beta traffic.
// ABOUTME: Generates internal sidecar aliases that cannot be selected by client headers.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutingProfile {
    Production,
    Beta,
}

impl RoutingProfile {
    pub const ALL: [Self; 2] = [Self::Production, Self::Beta];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Beta => "beta",
        }
    }

    pub fn sidecar_alias(self, canonical_model: &str) -> String {
        format!("seren-profile-{self}/{canonical_model}")
    }
}

impl fmt::Display for RoutingProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_aliases_are_profile_scoped() {
        assert_eq!(
            RoutingProfile::Production.sidecar_alias("vendor/model"),
            "seren-profile-production/vendor/model"
        );
        assert_eq!(
            RoutingProfile::Beta.sidecar_alias("vendor/model"),
            "seren-profile-beta/vendor/model"
        );
    }
}
