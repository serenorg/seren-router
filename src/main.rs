use anyhow::Context;
use seren_router::{
    config::RouterConfig, db, gateway_auth::GatewayAuth, pricing::PriceTable, proxy::ProxyState,
    registry::Registry, routes, server,
};

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
    let config = RouterConfig::from_env().context("invalid router configuration")?;
    let auth = GatewayAuth::new(config.gateway_key());
    let registry_bytes = std::fs::read(config.registry_path()).with_context(|| {
        format!(
            "failed to read provider registry {}",
            config.registry_path().display()
        )
    })?;
    let registry: Registry = serde_yaml::from_slice(&registry_bytes).with_context(|| {
        format!(
            "failed to parse provider registry {}",
            config.registry_path().display()
        )
    })?;
    registry
        .validate()
        .context("provider registry validation failed")?;
    let price_table =
        PriceTable::from_registry(&registry).context("provider registry pricing is invalid")?;
    let proxy = ProxyState::new(config.sidecar_url(), price_table)
        .context("invalid sidecar proxy configuration")?;
    let app = routes::public_router().merge(routes::protected_router(auth, proxy));
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
