use anyhow::Context;
use seren_router::{config::RouterConfig, db, gateway_auth::GatewayAuth, routes, server};

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
    let app = routes::public_router().merge(routes::protected_router(auth));
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
