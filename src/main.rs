use anyhow::Context;
use seren_router::{
    catalog::Catalog,
    config::{RouterConfig, registry_path_from_env},
    db, deployment,
    gateway_auth::GatewayAuth,
    ledger::Ledger,
    policy::measurements::MeasurementStore,
    pricing::PriceTable,
    proxy::ProxyState,
    registry::Registry,
    routes, server,
};
use std::ffi::OsString;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    server::setup_tracing("seren_router=debug,tower_http=debug");

    if let Err(error) = dispatch(std::env::args_os().skip(1)).await {
        tracing::error!(error = %error, error_chain = ?error, "startup failed");
        return Err(error);
    }

    Ok(())
}

enum Command {
    Serve,
    RenderSidecarConfig { output_path: PathBuf },
}

fn parse_command(args: impl IntoIterator<Item = OsString>) -> anyhow::Result<Command> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Ok(Command::Serve);
    };
    if command != "render-sidecar-config" {
        anyhow::bail!("unsupported command");
    }
    let output_path = args
        .next()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .context("render-sidecar-config requires exactly one output path")?;
    if args.next().is_some() {
        anyhow::bail!("render-sidecar-config requires exactly one output path");
    }
    Ok(Command::RenderSidecarConfig { output_path })
}

async fn dispatch(args: impl IntoIterator<Item = OsString>) -> anyhow::Result<()> {
    match parse_command(args)? {
        Command::Serve => run_service().await,
        Command::RenderSidecarConfig { output_path } => {
            let registry_path =
                registry_path_from_env().context("invalid registry configuration")?;
            deployment::render_sidecar_config(&registry_path, &output_path)
                .context("failed to render AgentGateway configuration")?;
            tracing::info!(
                registry = %registry_path.display(),
                output = %output_path.display(),
                "rendered AgentGateway configuration"
            );
            Ok(())
        }
    }
}

async fn run_service() -> anyhow::Result<()> {
    let config = RouterConfig::from_env().context("invalid router configuration")?;
    let auth = match config.beta_gateway_key() {
        Some(beta_key) => GatewayAuth::new(config.gateway_key()).with_beta_key(beta_key),
        None => GatewayAuth::new(config.gateway_key()),
    };
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
    let pool = db::connect_lazy(database_url.trim()).context("invalid DATABASE_URL")?;
    let database_health = db::DatabaseHealth::starting();
    tokio::spawn(db::supervise(pool.clone(), database_health.clone()));
    let ledger = Ledger::with_health(pool, database_health.clone());
    let proxy = ProxyState::new(
        config.sidecar_url(),
        price_table,
        ledger.clone(),
        &registry,
        config.routing(),
        MeasurementStore::default(),
    )
    .context("invalid sidecar proxy configuration")?;
    let app = routes::public_router().merge(routes::protected_router(auth, proxy, ledger, catalog));

    server::serve(app, database_health, config.sidecar_readiness_url().clone())
        .await
        .context("HTTP server exited with error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_parser_keeps_serve_default_and_requires_one_render_output() {
        assert!(matches!(
            parse_command(Vec::<OsString>::new()).unwrap(),
            Command::Serve
        ));
        assert!(matches!(
            parse_command([
                OsString::from("render-sidecar-config"),
                OsString::from("/config/agentgateway.yaml")
            ])
            .unwrap(),
            Command::RenderSidecarConfig { output_path }
                if output_path == std::path::Path::new("/config/agentgateway.yaml")
        ));
        assert!(parse_command([OsString::from("render-sidecar-config")]).is_err());
        assert!(
            parse_command([
                OsString::from("render-sidecar-config"),
                OsString::from("one"),
                OsString::from("two")
            ])
            .is_err()
        );
        assert!(parse_command([OsString::from("unknown")]).is_err());
    }
}
