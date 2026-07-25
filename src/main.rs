use anyhow::Context;
use seren_router::{
    catalog::Catalog, config::RouterConfig, db, gateway_auth::GatewayAuth, ledger::Ledger,
    pricing::PriceTable, proxy::ProxyState, registry::Registry, routes, server,
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
    let catalog = Catalog::from_registry(&registry);
    let price_table =
        PriceTable::from_registry(&registry).context("provider registry pricing is invalid")?;
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    if database_url.trim().is_empty() {
        anyhow::bail!("DATABASE_URL must not be empty");
    }
    let pool = db::connect(database_url.trim())
        .await
        .context("failed to connect to database")?;
    sqlx::migrate!()
        .run(&pool)
        .await
        .context("failed to migrate database")?;
    let ledger = Ledger::new(pool.clone());
    let proxy = ProxyState::new(config.sidecar_url(), price_table, ledger.clone())
        .context("invalid sidecar proxy configuration")?;
    let app = routes::public_router().merge(routes::protected_router(auth, proxy, ledger, catalog));

    server::serve(app, Some(pool))
        .await
        .context("HTTP server exited with error")
}
