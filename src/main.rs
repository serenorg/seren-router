use anyhow::Context;
use seren_router::{db, routes, server};

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
    // The protected registry is empty until M2 adds the Gateway's static bearer
    // middleware and inference handlers. Keeping both builders in production
    // composition prevents route-list drift when that surface lands.
    let app = routes::public_router().merge(routes::protected_router());
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
