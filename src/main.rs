use std::sync::Arc;

use anyhow::{Context, bail};
use axum::middleware::{from_fn, from_fn_with_state};
use seren_router::auth::{
    JwtVerifier, require_seren_passthrough_headers, verify_seren_identity_token,
};
use seren_router::{db, routes, server};

const DEFAULT_SERENCORE_URL: &str = "http://serencore:8080";
const JWT_AUDIENCE_ENV: &str = "SEREN_ROUTER_JWT_AUDIENCE";
const HEADER_AUTH_ENV: &str = "SEREN_ROUTER_ALLOW_INSECURE_HEADER_AUTH";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    server::setup_tracing("seren_router=debug,tower_http=debug");

    if let Err(error) = run().await {
        tracing::error!(error = %error, error_chain = ?error, "startup failed");
        return Err(error);
    }

    Ok(())
}

async fn run() -> anyhow::Result<()> {
    let serencore_url =
        std::env::var("SERENCORE_URL").unwrap_or_else(|_| DEFAULT_SERENCORE_URL.to_string());
    let protected = routes::protected_router();
    let protected = if let Some(verifier) = JwtVerifier::from_env(&serencore_url, JWT_AUDIENCE_ENV)
    {
        tracing::info!("Seren identity token verification enabled");
        protected.layer(from_fn_with_state(
            Arc::new(verifier),
            verify_seren_identity_token,
        ))
    } else if insecure_header_auth_enabled() {
        tracing::warn!(
            env = HEADER_AUTH_ENV,
            "using insecure trusted-header authentication"
        );
        protected.layer(from_fn(require_seren_passthrough_headers))
    } else {
        bail!(
            "SERENCORE_JWT_ISSUER and {JWT_AUDIENCE_ENV} are required; set {HEADER_AUTH_ENV}=true only for local development"
        );
    };
    let app = routes::public_router().merge(protected);
    let db = match std::env::var("DATABASE_URL") {
        Ok(raw) => {
            let url = raw.trim();
            if url.is_empty() {
                None
            } else {
                tracing::info!(
                    "database URL configured; readiness will check database connectivity"
                );
                Some(db::connect_lazy(url).context("invalid DATABASE_URL")?)
            }
        }
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => return Err(anyhow::Error::new(error).context("failed to read DATABASE_URL")),
    };

    server::serve(app, db)
        .await
        .context("HTTP server exited with error")
}

fn insecure_header_auth_enabled() -> bool {
    environment_flag(HEADER_AUTH_ENV)
}

fn environment_flag(name: &str) -> bool {
    environment_flag_value(std::env::var(name).ok().as_deref())
}

fn environment_flag_value(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

#[cfg(test)]
mod tests {
    use super::environment_flag_value;

    #[test]
    fn insecure_header_auth_requires_explicit_true() {
        assert!(environment_flag_value(Some("true")));
        assert!(environment_flag_value(Some("TRUE")));
        assert!(!environment_flag_value(None));
        assert!(!environment_flag_value(Some("false")));
        assert!(!environment_flag_value(Some("1")));
        assert!(!environment_flag_value(Some(" true ")));
    }
}
