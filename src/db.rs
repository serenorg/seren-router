//! Database pool helpers tuned for SerenDB scale-to-zero compute.
//!
//! SerenDB compute endpoints suspend after ~5 minutes of inactivity. The first
//! connection after suspend has to wait for the compute to wake (image pull,
//! Postgres + pgbouncer startup, proxy SCRAM handshake), which routinely takes
//! 30s+. The sqlx default `acquire_timeout` is 30s, so a service that starts
//! up against a cold compute will see `pool timed out while waiting for an
//! open connection` and exit, sending the pod into CrashLoopBackOff.
//!
//! The helpers in this module return a `PgPool` configured to ride out cold
//! starts without holding connections that would prevent the compute from
//! suspending again.

use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{str::FromStr, time::Duration};

/// Maximum time `pool.acquire()` will wait, including time to open a new
/// connection. Sized to cover a SerenDB cold-start (image pull + postgres
/// boot + proxy handshake) with headroom.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(90);

/// Drop idle connections quickly so the SerenDB compute can observe the
/// endpoint as idle and suspend. Holding a long-lived idle connection
/// would defeat scale-to-zero.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Recycle long-lived connections to avoid surprises with proxy/auth state.
const MAX_LIFETIME: Duration = Duration::from_secs(30 * 60);

/// Default upper bound on pool size. Override via [`options`] if a service
/// needs more headroom.
const MAX_CONNECTIONS: u32 = 10;

/// Build the [`PgPoolOptions`] used by the helpers below.
///
/// Exposed so callers that need to override a single field (e.g. raise
/// `max_connections`) can keep the rest of the cold-start-tolerant defaults.
pub fn options() -> PgPoolOptions {
    PgPoolOptions::new()
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(IDLE_TIMEOUT)
        .max_lifetime(MAX_LIFETIME)
        .min_connections(0)
        .max_connections(MAX_CONNECTIONS)
}

/// Open a pool, waiting up to [`ACQUIRE_TIMEOUT`] for the first connection.
///
/// Use this when the service should refuse to start if the database is
/// unreachable (most services). Cold-start latency is absorbed by the bumped
/// timeout, so a suspended SerenDB compute will not crash startup.
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    let opts = PgConnectOptions::from_str(database_url)?;
    options().connect_with(opts).await
}

/// Open a pool without contacting the database. The first query (or readiness
/// probe) drives the actual connect, so startup is unconditionally fast and a
/// transient cold-start during pod boot cannot crash the process.
pub fn connect_lazy(database_url: &str) -> Result<PgPool, sqlx::Error> {
    let opts = PgConnectOptions::from_str(database_url)?;
    Ok(options().connect_lazy_with(opts))
}
