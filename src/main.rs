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
    routes, server, upstream_catalog,
};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

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
            let registry = load_registry(&registry_path).await?;
            deployment::write_sidecar_config(&registry, &output_path)
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

/// Read the reviewed registry and extend every enabled provider that declares a
/// `catalog_url` with its upstream coverage.
///
/// The sidecar renderer and the service both load through here so a slug that is
/// advertised and routed always has a matching generated sidecar route.
async fn load_registry(registry_path: &Path) -> anyhow::Result<Registry> {
    let registry_bytes = std::fs::read(registry_path).with_context(|| {
        format!(
            "failed to read provider registry {}",
            registry_path.display()
        )
    })?;
    let mut registry: Registry = serde_yaml::from_slice(&registry_bytes).with_context(|| {
        format!(
            "failed to parse provider registry {}",
            registry_path.display()
        )
    })?;
    hydrate_upstream_coverage(&mut registry).await;
    registry
        .validate()
        .context("provider registry validation failed")?;
    Ok(registry)
}

/// An unreachable upstream must not take the service down: coverage falls back
/// to the reviewed registry, which is exactly the pre-hydration behaviour.
async fn hydrate_upstream_coverage(registry: &mut Registry) {
    let sources: Vec<(String, String)> = registry
        .providers
        .iter()
        .filter(|provider| provider.enabled)
        .filter_map(|provider| {
            provider
                .catalog_url
                .clone()
                .map(|url| (provider.id.clone(), url))
        })
        .collect();

    for (provider_id, url) in sources {
        match upstream_catalog::fetch_catalog(&url).await {
            Ok(catalog) => {
                let advertised = catalog.advertised;
                let priced = catalog.mappings.len();
                let outcome =
                    upstream_catalog::hydrate_provider(registry, &provider_id, catalog.mappings);
                tracing::info!(
                    provider = %provider_id,
                    catalog_url = %url,
                    advertised,
                    priced,
                    added = outcome.added,
                    reviewed = outcome.explicit_retained,
                    "extended provider coverage from upstream catalog"
                );
            }
            Err(error) => {
                tracing::error!(
                    provider = %provider_id,
                    catalog_url = %url,
                    error = %error,
                    "upstream catalog unavailable; serving reviewed registry coverage only"
                );
            }
        }
    }
}

async fn run_service() -> anyhow::Result<()> {
    let config = RouterConfig::from_env().context("invalid router configuration")?;
    let auth = match config.beta_gateway_key() {
        Some(beta_key) => GatewayAuth::new(config.gateway_key()).with_beta_key(beta_key),
        None => GatewayAuth::new(config.gateway_key()),
    };
    let registry = load_registry(config.registry_path()).await?;
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
    let ledger =
        Ledger::with_health(pool, database_health.clone()).with_public_provider_aliases(&registry);
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
