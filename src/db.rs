//! Database pool and recovery helpers tuned for SerenDB scale-to-zero compute.
//!
//! SerenDB compute endpoints suspend after ~5 minutes of inactivity. The first
//! connection after suspend has to wait for the compute to wake (image pull,
//! Postgres + pgbouncer startup, proxy SCRAM handshake), which routinely takes
//! 30s+. Router inference must remain available while this happens, so database
//! initialization and recovery run independently of the HTTP listener.
//!
//! The pool deliberately holds no minimum connections and the recovery loop is
//! event-driven after initialization. It therefore does not keep a scale-to-zero
//! database awake.

use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    future::Future,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

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

const INITIAL_RECOVERY_BACKOFF: Duration = Duration::from_millis(250);
const MAX_RECOVERY_BACKOFF: Duration = Duration::from_secs(30);

#[cfg(feature = "metrics")]
pub(crate) const DATABASE_AVAILABLE_METRIC: &str = "seren_router_database_available";
#[cfg(feature = "metrics")]
pub(crate) const DATABASE_RECOVERY_ATTEMPTS_METRIC: &str =
    "seren_router_database_recovery_attempts_total";
#[cfg(feature = "metrics")]
pub(crate) const DATABASE_OPERATION_FAILURES_METRIC: &str =
    "seren_router_database_operation_failures_total";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DatabaseStatus {
    Starting = 0,
    Ready = 1,
    Degraded = 2,
}

impl DatabaseStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Ready => "ok",
            Self::Degraded => "degraded",
        }
    }
}

#[derive(Clone)]
pub struct DatabaseHealth {
    inner: Arc<DatabaseHealthInner>,
}

struct DatabaseHealthInner {
    status: AtomicU8,
    recovery_requested: Notify,
}

impl DatabaseHealth {
    pub fn starting() -> Self {
        Self::new(DatabaseStatus::Starting)
    }

    pub fn ready() -> Self {
        Self::new(DatabaseStatus::Ready)
    }

    fn new(status: DatabaseStatus) -> Self {
        Self {
            inner: Arc::new(DatabaseHealthInner {
                status: AtomicU8::new(status as u8),
                recovery_requested: Notify::new(),
            }),
        }
    }

    pub fn status(&self) -> DatabaseStatus {
        match self.inner.status.load(Ordering::Acquire) {
            0 => DatabaseStatus::Starting,
            1 => DatabaseStatus::Ready,
            2 => DatabaseStatus::Degraded,
            _ => unreachable!("database status is written only by DatabaseHealth"),
        }
    }

    pub fn report_ready(&self) {
        let previous = self
            .inner
            .status
            .swap(DatabaseStatus::Ready as u8, Ordering::AcqRel);
        self.record_availability();
        if previous != DatabaseStatus::Ready as u8 {
            tracing::info!("database is available");
        }
    }

    pub fn report_failure(&self, operation: &'static str, error: &impl std::fmt::Display) {
        let previous = self
            .inner
            .status
            .swap(DatabaseStatus::Degraded as u8, Ordering::AcqRel);
        self.record_availability();
        record_operation_failure(operation);
        if previous != DatabaseStatus::Degraded as u8 {
            tracing::warn!(operation, error = %error, "database became unavailable");
        }
        self.inner.recovery_requested.notify_one();
    }

    pub(crate) fn record_availability(&self) {
        #[cfg(feature = "metrics")]
        metrics::gauge!(DATABASE_AVAILABLE_METRIC).set(if self.status() == DatabaseStatus::Ready {
            1.0
        } else {
            0.0
        });
    }

    async fn wait_for_recovery_request(&self) {
        while self.status() == DatabaseStatus::Ready {
            self.inner.recovery_requested.notified().await;
        }
    }
}

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

/// Run migrations until PostgreSQL becomes available, then sleep until a ledger
/// operation reports another database failure.
///
/// The loop is intentionally event-driven after a successful migration so it
/// does not keep scale-to-zero compute awake.
pub async fn supervise(pool: PgPool, health: DatabaseHealth) {
    supervise_with(
        health,
        INITIAL_RECOVERY_BACKOFF,
        MAX_RECOVERY_BACKOFF,
        move || {
            let pool = pool.clone();
            async move {
                sqlx::migrate!()
                    .run(&pool)
                    .await
                    .map_err(|error| error.to_string())
            }
        },
    )
    .await;
}

async fn supervise_with<Recover, RecoveryFuture>(
    health: DatabaseHealth,
    initial_backoff: Duration,
    max_backoff: Duration,
    mut recover: Recover,
) where
    Recover: FnMut() -> RecoveryFuture,
    RecoveryFuture: Future<Output = Result<(), String>>,
{
    let mut recovery_backoff = initial_backoff;

    loop {
        record_recovery_attempt(health.status());
        match recover().await {
            Ok(()) => {
                health.report_ready();
                recovery_backoff = initial_backoff;
                health.wait_for_recovery_request().await;
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    retry_ms = recovery_backoff.as_millis(),
                    "database initialization or recovery failed"
                );
                health.report_failure("migration", &error);
                tokio::time::sleep(recovery_backoff).await;
                recovery_backoff = std::cmp::min(recovery_backoff.saturating_mul(2), max_backoff);
            }
        }
    }
}

#[cfg(feature = "metrics")]
pub(crate) fn describe_metrics() {
    metrics::describe_gauge!(
        DATABASE_AVAILABLE_METRIC,
        "Whether PostgreSQL is currently available to the asynchronous generation ledger."
    );
    metrics::describe_counter!(
        DATABASE_RECOVERY_ATTEMPTS_METRIC,
        "Database migration or recovery attempts."
    );
    metrics::describe_counter!(
        DATABASE_OPERATION_FAILURES_METRIC,
        "Failed database operations by bounded operation name."
    );
}

fn record_recovery_attempt(status: DatabaseStatus) {
    #[cfg(feature = "metrics")]
    metrics::counter!(
        DATABASE_RECOVERY_ATTEMPTS_METRIC,
        "phase" => match status {
            DatabaseStatus::Starting => "initial",
            DatabaseStatus::Ready | DatabaseStatus::Degraded => "recovery",
        }
    )
    .increment(1);

    #[cfg(not(feature = "metrics"))]
    let _ = status;
}

fn record_operation_failure(operation: &'static str) {
    #[cfg(feature = "metrics")]
    metrics::counter!(DATABASE_OPERATION_FAILURES_METRIC, "operation" => operation).increment(1);

    #[cfg(not(feature = "metrics"))]
    let _ = operation;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn health_transitions_are_shared_and_stable() {
        let health = DatabaseHealth::starting();
        let observer = health.clone();
        assert_eq!(observer.status(), DatabaseStatus::Starting);

        health.report_ready();
        assert_eq!(observer.status(), DatabaseStatus::Ready);

        let error = sqlx::Error::PoolTimedOut;
        observer.report_failure("insert_generation", &error);
        assert_eq!(health.status(), DatabaseStatus::Degraded);
    }

    #[tokio::test]
    async fn supervisor_recovers_startup_and_midstream_failures_then_stays_idle() {
        let health = DatabaseHealth::starting();
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed_attempts = attempts.clone();
        let supervisor_health = health.clone();
        let supervisor = tokio::spawn(async move {
            supervise_with(
                supervisor_health,
                Duration::from_millis(1),
                Duration::from_millis(2),
                move || {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if attempt == 0 {
                            Err("simulated unavailable database".to_owned())
                        } else {
                            Ok(())
                        }
                    }
                },
            )
            .await;
        });

        wait_for_status(&health, DatabaseStatus::Ready).await;
        assert_eq!(observed_attempts.load(Ordering::SeqCst), 2);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(
            observed_attempts.load(Ordering::SeqCst),
            2,
            "healthy supervision must be event-driven rather than polling"
        );

        health.report_failure("insert_generation", &sqlx::Error::PoolTimedOut);
        wait_for_status(&health, DatabaseStatus::Ready).await;
        assert_eq!(observed_attempts.load(Ordering::SeqCst), 3);

        supervisor.abort();
    }

    async fn wait_for_status(health: &DatabaseHealth, expected: DatabaseStatus) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while health.status() != expected {
            assert!(
                tokio::time::Instant::now() < deadline,
                "database health did not reach {expected:?}"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
}
