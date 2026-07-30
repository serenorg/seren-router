// ABOUTME: Exercises Modal Kimi K3 through the real beta router and pinned sidecar.
// ABOUTME: Blocks activation on branding, accounting, database, or spend-contract drift.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::time::Duration;

use axum::Router;
use futures::FutureExt;
use reqwest::header::{CONTENT_TYPE, HeaderMap};
use reqwest::{Client, StatusCode, Url};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use seren_router::attribution::SERVED_PROVIDER_HEADER;
use seren_router::catalog::Catalog;
use seren_router::config::RoutingConfig;
use seren_router::gateway_auth::GatewayAuth;
use seren_router::ledger::Ledger;
use seren_router::policy::measurements::MeasurementStore;
use seren_router::pricing::{BillingPrices, PriceTable, Usage, cost_usd, provider_cost_usd};
use seren_router::proxy::ProxyState;
use seren_router::registry::Registry;
use seren_router::routes;
use seren_router::routing_profile::RoutingProfile;
use seren_router::sidecar_config::{SidecarConfigOptions, compile};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{AssertSqlSafe, FromRow, PgPool};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, timeout};
use uuid::Uuid;

const PROVIDER_ID: &str = "modal";
const MODEL: &str = "moonshotai/kimi-k3";
const MODAL_KEY_ENV: &str = "SEREN_ROUTER_KEY_MODAL";
const BUDGET_ENV: &str = "SEREN_MODAL_MAX_SPEND_USD";
// The operator sets a fresh UUID only after reviewing current account billing.
// A durable claim marker makes reuse fail before any sidecar/provider request.
const BILLING_REVIEW_ID_ENV: &str = "SEREN_MODAL_BILLING_REVIEW_ID";
const BILLING_STATE_DIR_ENV: &str = "SEREN_MODAL_BILLING_STATE_DIR";
const DATABASE_URL_ENV: &str = "DATABASE_URL";
const MAX_APPROVED_SPEND_USD: &str = "5";
const PRODUCTION_GATEWAY_KEY: &str = "modal-contract-production-gateway-key";
const BETA_GATEWAY_KEY: &str = "modal-contract-beta-gateway-key";
const MAX_COMPLETION_TOKENS: u64 = 4;
const CONSERVATIVE_PROMPT_TOKENS: u64 = 4_096;
const INITIAL_REQUESTS: u64 = 1;
// Cache telemetry gets one paid repeat. A missing signal blocks this run; no
// automatic request is issued beyond the reviewed hard cap.
const CACHE_REPEAT_REQUESTS: u64 = 1;
const MAX_LOGICAL_REQUESTS: u64 = INITIAL_REQUESTS + CACHE_REPEAT_REQUESTS;
const SIDECAR_RETRIES_PER_LOGICAL_REQUEST: u64 = 2;
const ATTEMPTS_PER_LOGICAL_REQUEST: u64 = 1 + SIDECAR_RETRIES_PER_LOGICAL_REQUEST;
const RESERVED_ATTEMPT_CEILING: u64 = MAX_LOGICAL_REQUESTS * ATTEMPTS_PER_LOGICAL_REQUEST;
const CACHE_PROMPT_REPETITIONS: usize = 1_800;
const CACHE_PROMPT_BLOCK: &str = "a ";
const STARTUP_DEADLINE: Duration = Duration::from_secs(15);
const REQUEST_DEADLINE: Duration = Duration::from_secs(180);
const LEDGER_DEADLINE: Duration = Duration::from_secs(10);
static PAID_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Settings {
    modal_key: String,
    database_url: String,
    budget: Decimal,
    registry: Registry,
    prices: BillingPrices,
    public_provider_name: String,
    public_provider_tag: String,
    private_markers: PrivateMarkers,
    billing_review_id: Uuid,
    billing_state_dir: PathBuf,
    estimated_maximum_per_request: Decimal,
    reserved_gross_provider_cost_ceiling: Decimal,
}

impl Settings {
    fn load() -> Self {
        let modal_key = required_secret_env(MODAL_KEY_ENV);
        validate_proxy_token_shape(&modal_key);
        let database_url = required_database_url();
        let budget = required_decimal_env(BUDGET_ENV);
        let billing_review_id = required_billing_review_id();
        let billing_state_dir = required_billing_state_dir();
        let maximum: Decimal = MAX_APPROVED_SPEND_USD.parse().unwrap();
        assert!(
            budget > Decimal::ZERO && budget <= maximum,
            "{BUDGET_ENV} must be greater than zero and at most {MAX_APPROVED_SPEND_USD}"
        );

        let mut registry = checked_registry();
        let modal = registry
            .providers
            .iter()
            .find(|provider| provider.id == PROVIDER_ID)
            .cloned()
            .expect("the checked registry must contain the internal modal provider");
        assert!(modal.enabled, "the checked Modal route must be enabled");
        assert!(
            modal.supports(RoutingProfile::Beta),
            "the modal provider must support the beta profile"
        );
        assert!(
            modal.supports(RoutingProfile::Production),
            "the modal provider must support the production canary"
        );
        assert_eq!(
            modal.secret_env, MODAL_KEY_ENV,
            "the modal provider must use the dedicated credential environment variable"
        );
        let modal_mapping = modal
            .models
            .iter()
            .find(|mapping| mapping.slug == MODEL)
            .cloned()
            .expect("the modal provider must map the canonical Kimi K3 slug");
        assert!(
            !modal_mapping.request_constraints.supports_streaming,
            "the candidate must not regain streaming until Modal returns terminal usage"
        );
        let public_provider_name = modal
            .public_display_name
            .clone()
            .expect("the modal provider must have a neutral public display name");
        let public_provider_tag = modal
            .public_tag
            .clone()
            .expect("the modal provider must have a neutral public tag");
        assert_no_internal_brand(
            "public provider display name",
            public_provider_name.as_bytes(),
        );
        assert_no_internal_brand("public provider tag", public_provider_tag.as_bytes());
        assert_ne!(
            public_provider_name, modal.display_name,
            "the public provider name must not expose the internal display name"
        );
        let private_markers =
            PrivateMarkers::from_contract(&modal, &modal_mapping.provider_model_id, &modal_key);

        for provider in &mut registry.providers {
            provider.enabled = provider.id == PROVIDER_ID;
        }
        registry
            .validate()
            .expect("the Modal-only beta registry must validate");
        assert_eq!(
            registry
                .providers
                .iter()
                .filter(|provider| provider.enabled)
                .count(),
            1,
            "the paid gate must isolate Modal from every fallback provider"
        );

        let prices = PriceTable::from_registry(&registry)
            .expect("the Modal-only registry pricing must validate")
            .get(PROVIDER_ID, MODEL)
            .cloned()
            .expect("the Modal Kimi K3 price row must be enabled");
        assert!(
            prices.provider_cached_input_price_per_mtok.is_some(),
            "Modal activation requires a checked cached-input rate"
        );
        let estimated_maximum_per_request = cost_usd(
            &prices.provider_cost,
            &Usage {
                prompt_tokens: CONSERVATIVE_PROMPT_TOKENS,
                completion_tokens: MAX_COMPLETION_TOKENS,
            },
        );
        let reserved_gross_provider_cost_ceiling =
            Decimal::from(RESERVED_ATTEMPT_CEILING) * estimated_maximum_per_request;
        assert!(
            reserved_gross_provider_cost_ceiling <= budget,
            "the ${reserved_gross_provider_cost_ceiling} hard-cap reservation for {RESERVED_ATTEMPT_CEILING} possible provider attempts exceeds the ${budget} budget"
        );

        Self {
            modal_key,
            database_url,
            budget,
            registry,
            prices,
            public_provider_name,
            public_provider_tag,
            private_markers,
            billing_review_id,
            billing_state_dir,
            estimated_maximum_per_request,
            reserved_gross_provider_cost_ceiling,
        }
    }
}

#[derive(Clone)]
struct PrivateMarker {
    label: &'static str,
    value: Vec<u8>,
}

#[derive(Clone)]
struct PrivateMarkers {
    values: Vec<PrivateMarker>,
    provider_model_id: Vec<u8>,
}

impl PrivateMarkers {
    fn from_contract(
        provider: &seren_router::registry::Provider,
        provider_model_id: &str,
        secret: &str,
    ) -> Self {
        let base_url = Url::parse(&provider.base_url)
            .expect("the Modal provider base URL must be an absolute URL");
        let host = base_url
            .host_str()
            .expect("the Modal provider base URL must contain a host");
        let candidates = [
            ("internal provider id", provider.id.as_str()),
            (
                "internal provider display name",
                provider.display_name.as_str(),
            ),
            ("provider base URL", provider.base_url.as_str()),
            ("provider base URL host", host),
            (
                "provider secret environment name",
                provider.secret_env.as_str(),
            ),
            ("provider proxy token", secret),
        ];
        let mut values = Vec::new();
        for (label, value) in candidates {
            assert!(!value.trim().is_empty(), "{label} must not be blank");
            values.push(PrivateMarker {
                label,
                value: value.as_bytes().to_vec(),
            });
        }
        assert!(
            !provider_model_id.trim().is_empty(),
            "provider-native model id must not be blank"
        );
        let provider_model_lower = provider_model_id.to_ascii_lowercase();
        let canonical_model_lower = MODEL.to_ascii_lowercase();
        // If the native ID is already part of the reviewed public slug, raw
        // substring rejection would reject the canonical model itself. Keep it
        // as a structured marker and pin every public model field exactly.
        if !canonical_model_lower.contains(&provider_model_lower) {
            values.push(PrivateMarker {
                label: "provider-native model id",
                value: provider_model_id.as_bytes().to_vec(),
            });
        }
        for label in [
            "internal provider id",
            "internal provider display name",
            "provider base URL",
            "provider base URL host",
            "provider secret environment name",
            "provider proxy token",
        ] {
            assert!(
                values.iter().any(|marker| marker.label == label),
                "private markers must include {label}"
            );
        }
        Self {
            values,
            provider_model_id: provider_model_id.as_bytes().to_vec(),
        }
    }

    fn assert_absent(&self, context: &str, value: &[u8]) {
        let haystack = String::from_utf8_lossy(value).to_ascii_lowercase();
        for marker in &self.values {
            let needle = String::from_utf8_lossy(&marker.value).to_ascii_lowercase();
            assert!(
                !haystack.contains(&needle),
                "{context} exposed the private {}",
                marker.label
            );
        }
    }

    fn assert_canonical_model(&self, value: &Value, context: &str) {
        assert_eq!(
            value, MODEL,
            "{context} must expose only the canonical model slug"
        );
        let provider_model_id = String::from_utf8_lossy(&self.provider_model_id);
        if !provider_model_id.eq_ignore_ascii_case(MODEL) {
            assert_ne!(
                value.as_str(),
                Some(provider_model_id.as_ref()),
                "{context} exposed the provider-native model id"
            );
        }
    }
}

struct Ports {
    llm: u16,
    readiness: u16,
    admin: u16,
    stats: u16,
    router: u16,
    router_listener: StdTcpListener,
}

impl Ports {
    fn allocate() -> Self {
        let reservations: Vec<_> = (0..5)
            .map(|_| StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap())
            .collect();
        let values: Vec<_> = reservations
            .iter()
            .map(|listener| listener.local_addr().unwrap().port())
            .collect();
        assert_eq!(
            values.iter().copied().collect::<HashSet<_>>().len(),
            values.len(),
            "functional ports must be distinct"
        );
        let router_listener = reservations.last().unwrap().try_clone().unwrap();
        drop(reservations);

        Self {
            llm: values[0],
            readiness: values[1],
            admin: values[2],
            stats: values[3],
            router: values[4],
            router_listener,
        }
    }

    fn owned_runtime_ports(&self) -> [u16; 5] {
        [
            self.llm,
            self.readiness,
            self.admin,
            self.stats,
            self.router,
        ]
    }
}

struct Artifacts {
    directory: PathBuf,
    config: PathBuf,
    sidecar_log: PathBuf,
}

impl Artifacts {
    fn create() -> Self {
        let directory = std::env::temp_dir().join(format!("seren-router-modal-{}", Uuid::new_v4()));
        create_private_directory(&directory);
        Self {
            config: directory.join("agentgateway.yaml"),
            sidecar_log: directory.join("agentgateway.log"),
            directory,
        }
    }
}

impl Drop for Artifacts {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

struct OwnedChild {
    child: Child,
}

impl OwnedChild {
    fn try_wait(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().unwrap()
    }

    fn terminate(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

struct DatabaseSandbox {
    admin_pool: PgPool,
    pool: PgPool,
    schema: String,
}

impl DatabaseSandbox {
    async fn create(database_url: &str) -> Self {
        let options = PgConnectOptions::from_str(database_url)
            .unwrap_or_else(|_| panic!("{DATABASE_URL_ENV} must be a valid PostgreSQL URL"));
        let admin_pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(90))
            .connect_with(options.clone())
            .await
            .unwrap_or_else(|_| {
                panic!("{DATABASE_URL_ENV} must reach a disposable PostgreSQL database")
            });
        let schema = format!("modal_contract_{}", Uuid::new_v4().simple());
        // The identifier is generated locally from a fixed prefix and UUID hex.
        sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin_pool)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "{DATABASE_URL_ENV} must permit creation of an isolated disposable test schema"
                )
            });

        let pool = match PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(90))
            .connect_with(options.options([("search_path", schema.as_str())]))
            .await
        {
            Ok(pool) => pool,
            Err(_) => {
                drop_schema(&admin_pool, &schema).await;
                panic!("failed to connect to the isolated Modal contract schema");
            }
        };
        if sqlx::migrate!().run(&pool).await.is_err() {
            pool.close().await;
            drop_schema(&admin_pool, &schema).await;
            panic!("Modal contract database migrations must succeed");
        }

        Self {
            admin_pool,
            pool,
            schema,
        }
    }

    async fn cleanup(self) {
        self.pool.close().await;
        drop_schema(&self.admin_pool, &self.schema).await;
        self.admin_pool.close().await;
    }
}

async fn drop_schema(pool: &PgPool, schema: &str) {
    // `schema` is the fixed-prefix UUID identifier created by this test.
    sqlx::query(AssertSqlSafe(format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE"
    )))
    .execute(pool)
    .await
    .expect("the disposable Modal contract schema must be removable");
}

struct ModalHarness {
    client: Client,
    router_base_url: String,
    pool: PgPool,
    sidecar: OwnedChild,
    router_shutdown: Option<oneshot::Sender<()>>,
    router_task: Option<JoinHandle<std::io::Result<()>>>,
    runtime_ports: [u16; 5],
    _artifacts: Artifacts,
}

impl ModalHarness {
    async fn start(settings: &Settings, pool: PgPool) -> Self {
        assert_ne!(
            PRODUCTION_GATEWAY_KEY, BETA_GATEWAY_KEY,
            "the beta gate must use a credential distinct from production"
        );
        let ports = Ports::allocate();
        let runtime_ports = ports.owned_runtime_ports();
        let artifacts = Artifacts::create();
        let options = SidecarConfigOptions {
            llm_port: ports.llm,
            admin_addr: Some(loopback(ports.admin)),
            stats_addr: Some(loopback(ports.stats)),
            readiness_addr: loopback(ports.readiness),
            enable_ipv6: false,
        };
        let config = compile(&settings.registry, options)
            .expect("the checked Modal registry must compile for AgentGateway");
        let parsed_config: serde_yaml::Value =
            serde_yaml::from_slice(&config).expect("the rendered sidecar config must be YAML");
        assert_eq!(
            parsed_config["policies"][0]["policy"]["retry"]["attempts"].as_u64(),
            Some(SIDECAR_RETRIES_PER_LOGICAL_REQUEST),
            "the paid-attempt reservation must match AgentGateway retry configuration"
        );
        assert!(
            !String::from_utf8_lossy(&config).contains(&settings.modal_key),
            "the rendered sidecar configuration must reference, not contain, the Modal secret"
        );
        let mut config_file = create_private_file(&artifacts.config);
        config_file
            .write_all(&config)
            .expect("the private sidecar config must be writable");
        drop(config_file);

        let log = create_private_file(&artifacts.sidecar_log);
        let binary = sidecar_binary();
        // This proves only the test child's environment and private artifacts.
        // Deployment secret-mount isolation remains a separate deployment gate.
        let child = Command::new(&binary)
            .arg("-f")
            .arg(&artifacts.config)
            .env_clear()
            .env(MODAL_KEY_ENV, &settings.modal_key)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap_or_else(|_| panic!("failed to start {}", binary.display()));
        let mut sidecar = OwnedChild { child };
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(REQUEST_DEADLINE)
            .build()
            .unwrap();
        wait_for_sidecar(
            &client,
            &mut sidecar,
            ports.readiness,
            &artifacts.sidecar_log,
            &settings.modal_key,
        )
        .await;

        ports.router_listener.set_nonblocking(true).unwrap();
        let router_listener = tokio::net::TcpListener::from_std(ports.router_listener).unwrap();
        let ledger = Ledger::new(pool.clone()).with_public_provider_aliases(&settings.registry);
        let app = router_app(
            &format!("http://127.0.0.1:{}", ports.llm),
            &settings.registry,
            ledger,
        );
        let (router_shutdown, shutdown) = oneshot::channel();
        let router_task = tokio::spawn(async move {
            axum::serve(router_listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown.await;
                })
                .await
        });
        let router_base_url = format!("http://127.0.0.1:{}", ports.router);
        wait_for_router(&client, &router_base_url).await;

        Self {
            client,
            router_base_url,
            pool,
            sidecar,
            router_shutdown: Some(router_shutdown),
            router_task: Some(router_task),
            runtime_ports,
            _artifacts: artifacts,
        }
    }

    async fn completion(&self, prompt: &str) -> CompletionResponse {
        let body = json!({
            "model": MODEL,
            "messages": [{
                "role": "user",
                "content": prompt
            }],
            "top_p": 0.95,
            "max_tokens": MAX_COMPLETION_TOKENS,
            "stream": false,
            "provider": {"sort": "price"}
        });

        let started_at = Instant::now();
        let response = self
            .client
            .post(format!("{}/api/v1/chat/completions", self.router_base_url))
            .bearer_auth(BETA_GATEWAY_KEY)
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "beta completion outcome is unknown; review provider billing and use a fresh {BILLING_REVIEW_ID_ENV} before any rerun"
                )
            });
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .bytes()
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "beta completion body outcome is unknown; review provider billing and use a fresh {BILLING_REVIEW_ID_ENV} before any rerun"
                )
            })
            .to_vec();

        CompletionResponse {
            status,
            headers,
            body,
            elapsed: started_at.elapsed(),
        }
    }

    async fn public_generation(&self, id: &str) -> PublicResponse {
        let mut url = Url::parse(&format!("{}/api/v1/generation", self.router_base_url)).unwrap();
        url.query_pairs_mut().append_pair("id", id);
        let response = self
            .client
            .get(url)
            .bearer_auth(BETA_GATEWAY_KEY)
            .send()
            .await
            .expect("generation metadata transport must succeed");
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .bytes()
            .await
            .expect("generation metadata body must be readable")
            .to_vec();
        PublicResponse {
            status,
            headers,
            body,
        }
    }

    async fn catalog(&self, path: &str, key: &str) -> PublicResponse {
        let response = self
            .client
            .get(format!("{}{path}", self.router_base_url))
            .bearer_auth(key)
            .send()
            .await
            .expect("local catalog transport must succeed");
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .bytes()
            .await
            .expect("local catalog body must be readable")
            .to_vec();
        PublicResponse {
            status,
            headers,
            body,
        }
    }

    async fn ledger_row(&self, id: &str) -> LedgerRow {
        let deadline = Instant::now() + LEDGER_DEADLINE;
        loop {
            let row = sqlx::query_as::<_, LedgerRow>(
                r#"
                SELECT
                    routing_profile,
                    canonical_slug,
                    provider_id,
                    prompt_tokens,
                    completion_tokens,
                    provider_cost_usd,
                    sell_price_usd,
                    status
                FROM generations
                WHERE id = $1
                "#,
            )
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .expect("the isolated generation ledger query must succeed");
            if let Some(row) = row {
                return row;
            }
            assert!(
                Instant::now() < deadline,
                "the generation ledger insert did not complete before the activation deadline"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn shutdown_and_assert_clean(mut self) {
        self.shutdown_router().await;
        self.sidecar.terminate();

        for port in self.runtime_ports {
            StdTcpListener::bind((Ipv4Addr::LOCALHOST, port))
                .unwrap_or_else(|error| panic!("port {port} leaked after cleanup: {error}"));
        }
    }

    async fn shutdown_router(&mut self) {
        if let Some(shutdown) = self.router_shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(mut task) = self.router_task.take()
            && timeout(Duration::from_secs(2), &mut task).await.is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for ModalHarness {
    fn drop(&mut self) {
        if let Some(shutdown) = self.router_shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.router_task.take() {
            task.abort();
        }
    }
}

struct CompletionResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
    elapsed: Duration,
}

impl CompletionResponse {
    fn assert_success(
        &self,
        expected_content_type: &str,
        secret: &str,
        private_markers: &PrivateMarkers,
        context: &str,
    ) {
        assert_eq!(
            self.status,
            StatusCode::OK,
            "{context} completion failed; review provider billing and use a fresh {BILLING_REVIEW_ID_ENV} before any rerun. Body: {}",
            redacted_text(&self.body, secret)
        );
        assert!(
            self.headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with(expected_content_type)),
            "{context} returned an unexpected content type"
        );
        assert_eq!(
            self.headers.get(SERVED_PROVIDER_HEADER),
            None,
            "{context} exposed the internal served-provider header"
        );
        assert_public_headers(private_markers, &self.headers, context);
        private_markers.assert_absent(context, &self.body);
    }
}

struct PublicResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

#[derive(Debug, FromRow)]
struct LedgerRow {
    routing_profile: String,
    canonical_slug: String,
    provider_id: String,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    provider_cost_usd: Option<Decimal>,
    sell_price_usd: Option<Decimal>,
    status: i16,
}

#[derive(Clone, Copy)]
struct UsageObservation {
    usage: Usage,
    cached_prompt_tokens: Option<u64>,
    sell_price_usd: Decimal,
    provider_cost_usd: Option<Decimal>,
}

struct SpendGuard {
    logical_requests_started: u64,
    reserved_per_logical_request: Decimal,
    budget: Decimal,
}

impl SpendGuard {
    fn new(settings: &Settings) -> Self {
        Self {
            logical_requests_started: 0,
            reserved_per_logical_request: Decimal::from(ATTEMPTS_PER_LOGICAL_REQUEST)
                * settings.estimated_maximum_per_request,
            budget: settings.budget,
        }
    }

    fn start_logical_request(&mut self) {
        // AgentGateway does not expose attempt-level billing telemetry here. Keep
        // all initial-plus-retry reservations charged even after a successful
        // response so unknown timeout or failed attempts are never treated as free.
        assert!(
            self.logical_requests_started < MAX_LOGICAL_REQUESTS,
            "automatic paid probes are capped; review provider billing and use a fresh {BILLING_REVIEW_ID_ENV} before any rerun"
        );
        let next_count = self.logical_requests_started + 1;
        let reserved_after_next = Decimal::from(next_count) * self.reserved_per_logical_request;
        assert!(
            reserved_after_next <= self.budget,
            "the next logical request reserves ${reserved_after_next} for worst-case provider attempts, above the ${} budget",
            self.budget
        );
        self.logical_requests_started = next_count;
    }

    fn assert_complete(&self) {
        assert_eq!(
            self.logical_requests_started, MAX_LOGICAL_REQUESTS,
            "the gate must exercise exactly the bounded logical request count"
        );
    }
}

#[tokio::test]
#[ignore = "spends real provider budget and requires a disposable PostgreSQL database"]
async fn modal_kimi_beta_contract() {
    let _paid_test_guard = PAID_TEST_LOCK.lock().await;
    let settings = Settings::load();
    let cumulative_reserved_gross_cost = claim_billing_review(
        &settings.billing_state_dir,
        settings.billing_review_id,
        settings.reserved_gross_provider_cost_ceiling,
    );
    let database = DatabaseSandbox::create(&settings.database_url).await;
    let result = AssertUnwindSafe(run_contract(
        &settings,
        database.pool.clone(),
        cumulative_reserved_gross_cost,
    ))
    .catch_unwind()
    .await;
    database.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn run_contract(settings: &Settings, pool: PgPool, cumulative_reserved_gross_cost: Decimal) {
    let harness = ModalHarness::start(settings, pool).await;
    assert_catalog_contract(&harness, settings).await;
    let prompt = deterministic_cache_prompt();
    let mut spend_guard = SpendGuard::new(settings);
    let mut resolved_gross_cost = Decimal::ZERO;
    let mut observed_sell_price = Decimal::ZERO;
    let mut prompt_tokens = 0_u64;
    let mut completion_tokens = 0_u64;
    let mut cached_prompt_tokens = 0_u64;

    spend_guard.start_logical_request();
    let json_response = harness.completion(&prompt).await;
    json_response.assert_success(
        "application/json",
        &settings.modal_key,
        &settings.private_markers,
        "JSON",
    );
    let json_body: Value =
        serde_json::from_slice(&json_response.body).expect("JSON completion must be valid JSON");
    assert_json_completion_shape(&json_body, &settings.private_markers);
    let json_usage = usage_observation(&json_body, &settings.prices, "JSON");
    let json_id = response_id(&json_body, "JSON");
    assert_generation_contract(&harness, settings, json_id, json_usage, "JSON").await;
    resolved_gross_cost += json_usage.provider_cost_usd.unwrap_or(Decimal::ZERO);
    observed_sell_price += json_usage.sell_price_usd;
    prompt_tokens += json_usage.usage.prompt_tokens;
    completion_tokens += json_usage.usage.completion_tokens;
    cached_prompt_tokens += json_usage.cached_prompt_tokens.unwrap_or_default();

    spend_guard.start_logical_request();
    let repeat_json_response = harness.completion(&prompt).await;
    repeat_json_response.assert_success(
        "application/json",
        &settings.modal_key,
        &settings.private_markers,
        "repeat JSON",
    );
    let repeat_json_body: Value = serde_json::from_slice(&repeat_json_response.body)
        .expect("repeat JSON completion must be valid JSON");
    assert_json_completion_shape(&repeat_json_body, &settings.private_markers);
    let repeat_json_usage = usage_observation(&repeat_json_body, &settings.prices, "repeat JSON");
    let repeat_cached_prompt_tokens = repeat_json_usage
        .cached_prompt_tokens
        .filter(|cached_tokens| *cached_tokens > 0)
        .unwrap_or_else(|| {
            panic!(
                "repeat JSON must report a positive cached-token count; automatic paid probes are capped, so review provider billing and use a fresh {BILLING_REVIEW_ID_ENV} before any rerun"
            )
        });
    let repeat_provider_cost = repeat_json_usage
        .provider_cost_usd
        .expect("repeat JSON cached usage must resolve an exact provider cost");
    assert_eq!(
        repeat_json_usage.usage.prompt_tokens, json_usage.usage.prompt_tokens,
        "the two identical prompts must have the same prompt-token count"
    );
    let repeat_json_id = response_id(&repeat_json_body, "repeat JSON");
    assert_generation_contract(
        &harness,
        settings,
        repeat_json_id,
        repeat_json_usage,
        "repeat JSON",
    )
    .await;
    resolved_gross_cost += repeat_provider_cost;
    observed_sell_price += repeat_json_usage.sell_price_usd;
    prompt_tokens += repeat_json_usage.usage.prompt_tokens;
    completion_tokens += repeat_json_usage.usage.completion_tokens;
    cached_prompt_tokens += repeat_cached_prompt_tokens;
    spend_guard.assert_complete();

    println!(
        "modal_beta_contract logical_requests={} reserved_attempt_ceiling={RESERVED_ATTEMPT_CEILING} errors=0 json_ms={:.3} repeat_json_ms={:.3} prompt_tokens={prompt_tokens} cached_prompt_tokens={cached_prompt_tokens} completion_tokens={completion_tokens} resolved_gross_provider_cost={resolved_gross_cost} reserved_gross_provider_cost_ceiling={} cumulative_reserved_gross_provider_cost={} cumulative_approved_spend={} sell_subtotal={observed_sell_price} run_budget={}",
        spend_guard.logical_requests_started,
        milliseconds(json_response.elapsed),
        milliseconds(repeat_json_response.elapsed),
        settings.reserved_gross_provider_cost_ceiling,
        cumulative_reserved_gross_cost,
        MAX_APPROVED_SPEND_USD,
        settings.budget,
    );

    // Tool calling and structured-output probes remain a separate manual validation.
    // Neither capability is pinned in the checked registry, and adding ambiguous paid
    // requests would weaken this deterministic two-request activation gate.
    harness.shutdown_and_assert_clean().await;
}

async fn assert_catalog_contract(harness: &ModalHarness, settings: &Settings) {
    let models_path = "/api/v1/models";
    let endpoints_path = format!("/api/v1/models/{MODEL}/endpoints");

    let production_models = harness.catalog(models_path, PRODUCTION_GATEWAY_KEY).await;
    assert_eq!(
        production_models.status,
        StatusCode::OK,
        "production model catalog must remain available"
    );
    assert_public_response(
        &production_models,
        &settings.private_markers,
        "production model catalog",
    );
    let production_models_body: Value = serde_json::from_slice(&production_models.body)
        .expect("production model catalog must be JSON");
    assert!(
        production_models_body["data"]
            .as_array()
            .is_some_and(|models| models.iter().all(|model| model["id"] != MODEL)),
        "production catalog must not expose the isolated beta model"
    );

    let production_endpoints = harness
        .catalog(&endpoints_path, PRODUCTION_GATEWAY_KEY)
        .await;
    assert_eq!(
        production_endpoints.status,
        StatusCode::NOT_FOUND,
        "production must not resolve the isolated beta endpoint"
    );
    assert_public_response(
        &production_endpoints,
        &settings.private_markers,
        "production endpoint catalog",
    );

    let beta_models = harness.catalog(models_path, BETA_GATEWAY_KEY).await;
    assert_eq!(
        beta_models.status,
        StatusCode::OK,
        "beta model catalog must be available"
    );
    assert_public_response(
        &beta_models,
        &settings.private_markers,
        "beta model catalog",
    );
    let beta_models_body: Value =
        serde_json::from_slice(&beta_models.body).expect("beta model catalog must be JSON");
    assert_eq!(
        beta_models_body["data"]
            .as_array()
            .expect("beta model catalog data must be an array")
            .iter()
            .filter(|model| model["id"] == MODEL)
            .count(),
        1,
        "beta model catalog must list the canonical model exactly once"
    );

    let beta_endpoints = harness.catalog(&endpoints_path, BETA_GATEWAY_KEY).await;
    assert_eq!(
        beta_endpoints.status,
        StatusCode::OK,
        "beta endpoint catalog must expose the isolated route"
    );
    assert_public_response(
        &beta_endpoints,
        &settings.private_markers,
        "beta endpoint catalog",
    );
    let beta_endpoints_body: Value =
        serde_json::from_slice(&beta_endpoints.body).expect("beta endpoint catalog must be JSON");
    assert_eq!(beta_endpoints_body["data"]["id"], MODEL);
    let endpoints = beta_endpoints_body["data"]["endpoints"]
        .as_array()
        .expect("beta endpoint catalog endpoints must be an array");
    assert_eq!(
        endpoints.len(),
        1,
        "the isolated registry must expose one beta endpoint"
    );
    settings
        .private_markers
        .assert_canonical_model(&endpoints[0]["model_id"], "beta endpoint model");
    assert_eq!(
        endpoints[0]["provider_name"], settings.public_provider_name,
        "beta endpoint must use the neutral provider alias"
    );
    assert_eq!(
        endpoints[0]["tag"], settings.public_provider_tag,
        "beta endpoint must use the neutral provider tag"
    );
}

fn assert_public_response(
    response: &PublicResponse,
    private_markers: &PrivateMarkers,
    context: &str,
) {
    assert_public_headers(private_markers, &response.headers, context);
    private_markers.assert_absent(context, &response.body);
}

async fn assert_generation_contract(
    harness: &ModalHarness,
    settings: &Settings,
    id: &str,
    usage: UsageObservation,
    context: &str,
) {
    let row = harness.ledger_row(id).await;
    assert_eq!(row.routing_profile, "beta", "{context} routing profile");
    assert_eq!(row.canonical_slug, MODEL, "{context} canonical model");
    assert_eq!(row.provider_id, PROVIDER_ID, "{context} internal provider");
    assert_eq!(
        row.prompt_tokens,
        Some(i64::try_from(usage.usage.prompt_tokens).unwrap()),
        "{context} prompt tokens"
    );
    assert_eq!(
        row.completion_tokens,
        Some(i64::try_from(usage.usage.completion_tokens).unwrap()),
        "{context} completion tokens"
    );
    match usage.provider_cost_usd {
        Some(expected_provider_cost) => {
            assert!(
                expected_provider_cost > Decimal::ZERO,
                "{context} resolved provider cost must be positive"
            );
            assert_eq!(
                row.provider_cost_usd,
                Some(expected_provider_cost),
                "{context} exact provider cost"
            );
        }
        None => assert_eq!(
            row.provider_cost_usd, None,
            "{context} unresolved cold-cache provider cost must remain NULL, never a guessed zero"
        ),
    }
    assert_eq!(
        row.sell_price_usd,
        Some(usage.sell_price_usd),
        "{context} exact customer sell price"
    );
    assert_eq!(row.status, 200, "{context} ledger status");

    let public = harness.public_generation(id).await;
    assert_eq!(
        public.status,
        StatusCode::OK,
        "{context} public generation metadata lookup failed"
    );
    assert_public_headers(
        &settings.private_markers,
        &public.headers,
        &format!("{context} generation metadata"),
    );
    settings
        .private_markers
        .assert_absent(&format!("{context} generation metadata"), &public.body);
    let body: Value =
        serde_json::from_slice(&public.body).expect("generation metadata must be valid JSON");
    assert_eq!(body["data"]["id"], id, "{context} generation id");
    settings.private_markers.assert_canonical_model(
        &body["data"]["model"],
        &format!("{context} generation model"),
    );
    assert_eq!(
        body["data"]["provider_name"], settings.public_provider_name,
        "{context} generation provider name must use the neutral public alias"
    );
    assert_eq!(
        decimal_at(&body["data"]["total_cost"], "generation total_cost"),
        usage.sell_price_usd,
        "{context} public generation sell price"
    );
}

fn usage_observation(value: &Value, prices: &BillingPrices, context: &str) -> UsageObservation {
    let usage_value = value
        .get("usage")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{context} response must contain an OpenAI usage object"));
    let usage = Usage {
        prompt_tokens: usage_value
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                panic!("{context} usage.prompt_tokens must be a nonnegative integer")
            }),
        completion_tokens: usage_value
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                panic!("{context} usage.completion_tokens must be a nonnegative integer")
            }),
    };
    assert!(
        usage.prompt_tokens > 0,
        "{context} prompt token count must be positive"
    );
    assert!(
        usage.prompt_tokens <= CONSERVATIVE_PROMPT_TOKENS,
        "{context} prompt usage exceeded the conservative preflight estimate"
    );
    assert!(
        usage.completion_tokens <= MAX_COMPLETION_TOKENS,
        "{context} completion usage exceeded max_tokens"
    );
    let cached_prompt_tokens = optional_cached_prompt_tokens(usage_value, context);
    if let Some(cached_prompt_tokens) = cached_prompt_tokens {
        assert!(
            cached_prompt_tokens <= usage.prompt_tokens,
            "{context} cached tokens exceed total prompt tokens"
        );
    }

    let sell_price_usd = cost_usd(&prices.sell_price, &usage);
    assert_eq!(
        decimal_at(
            usage_value
                .get("cost")
                .expect("usage.cost must be injected"),
            "usage.cost"
        ),
        sell_price_usd,
        "{context} exact customer sell cost"
    );
    let provider_cost_usd = provider_cost_usd(prices, &usage, cached_prompt_tokens);
    let uncached_provider_cost_upper_bound_usd = cost_usd(&prices.provider_cost, &usage);
    if let Some(provider_cost_usd) = provider_cost_usd {
        assert!(
            provider_cost_usd > Decimal::ZERO,
            "{context} resolved provider cost must be positive"
        );
        assert!(
            provider_cost_usd <= uncached_provider_cost_upper_bound_usd,
            "{context} cached provider cost exceeds the uncached upper bound"
        );
    }

    UsageObservation {
        usage,
        cached_prompt_tokens,
        sell_price_usd,
        provider_cost_usd,
    }
}

fn optional_cached_prompt_tokens(
    usage: &serde_json::Map<String, Value>,
    context: &str,
) -> Option<u64> {
    let details = match usage.get("prompt_tokens_details") {
        None | Some(Value::Null) => return None,
        Some(Value::Object(details)) => details,
        Some(_) => panic!("{context} usage.prompt_tokens_details must be an object when present"),
    };
    match details.get("cached_tokens") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .unwrap_or_else(|| panic!("{context} cached_tokens must be a nonnegative integer")),
        ),
    }
}

fn deterministic_cache_prompt() -> String {
    let repeated = CACHE_PROMPT_BLOCK.repeat(CACHE_PROMPT_REPETITIONS);
    let prompt = format!(
        "Read this deterministic cache-validation prefix carefully.\n{repeated}\nReply with exactly one lowercase word: pong"
    );
    let word_count = prompt.split_whitespace().count();
    assert!(
        (1_700..2_000).contains(&word_count),
        "cache-validation prompt must remain long but below the conservative token estimate"
    );
    assert!(
        prompt.len() < usize::try_from(CONSERVATIVE_PROMPT_TOKENS).unwrap(),
        "ASCII cache-validation prompt bytes must fit the conservative token estimate"
    );
    prompt
}

fn assert_json_completion_shape(value: &Value, private_markers: &PrivateMarkers) {
    assert!(value.is_object(), "JSON completion must be an object");
    response_id(value, "JSON");
    assert!(
        value["object"]
            .as_str()
            .is_some_and(|object| object.starts_with("chat.completion")),
        "JSON completion must use the OpenAI chat-completion object shape"
    );
    private_markers.assert_canonical_model(&value["model"], "JSON completion model");
    let choices = value["choices"]
        .as_array()
        .filter(|choices| !choices.is_empty())
        .expect("JSON completion must contain at least one choice");
    let first = &choices[0];
    assert!(
        first["index"].as_u64().is_some(),
        "JSON completion choice must contain an index"
    );
    assert!(
        first["message"].is_object(),
        "JSON completion choice must contain an OpenAI message"
    );
    assert_eq!(
        first["message"]["role"], "assistant",
        "JSON completion message role"
    );
}

fn response_id<'a>(value: &'a Value, context: &str) -> &'a str {
    value["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| panic!("{context} response must contain a nonempty id"))
}

fn decimal_at(value: &Value, field: &str) -> Decimal {
    assert!(value.is_number(), "{field} must be a JSON number");
    value
        .to_string()
        .parse()
        .unwrap_or_else(|_| panic!("{field} must be an exact decimal USD amount"))
}

fn checked_registry() -> Registry {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("registry/providers.yaml");
    let bytes = fs::read(path).expect("the checked provider registry must be readable");
    let registry: Registry =
        serde_yaml::from_slice(&bytes).expect("the checked provider registry must parse");
    registry
        .validate()
        .expect("the checked provider registry must validate");
    registry
}

fn router_app(sidecar_url: &str, registry: &Registry, ledger: Ledger) -> Router {
    let auth = GatewayAuth::new(PRODUCTION_GATEWAY_KEY.as_bytes())
        .with_beta_key(BETA_GATEWAY_KEY.as_bytes());
    let catalog = Catalog::from_registry(registry);
    let proxy = ProxyState::new(
        sidecar_url,
        PriceTable::from_registry(registry).unwrap(),
        ledger.clone(),
        registry,
        RoutingConfig::new("1000".parse().unwrap(), 0.1, Decimal::ONE, 100).unwrap(),
        MeasurementStore::default(),
    )
    .unwrap();
    routes::public_router().merge(routes::protected_router(auth, proxy, ledger, catalog))
}

async fn wait_for_sidecar(
    client: &Client,
    child: &mut OwnedChild,
    readiness_port: u16,
    log_path: &Path,
    secret: &str,
) {
    let deadline = Instant::now() + STARTUP_DEADLINE;
    let mut poll = tokio::time::interval(Duration::from_millis(50));
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let readiness = format!("http://127.0.0.1:{readiness_port}/healthz/ready");

    loop {
        poll.tick().await;
        if let Some(status) = child.try_wait() {
            let log = redacted_log(log_path, secret);
            panic!("agentgateway exited during startup with {status}\n{log}");
        }
        if let Ok(response) = client.get(&readiness).send().await
            && response.status().is_success()
        {
            return;
        }
        if Instant::now() >= deadline {
            let log = redacted_log(log_path, secret);
            panic!("agentgateway readiness deadline exceeded\n{log}");
        }
    }
}

async fn wait_for_router(client: &Client, base_url: &str) {
    let deadline = Instant::now() + STARTUP_DEADLINE;
    let mut poll = tokio::time::interval(Duration::from_millis(50));
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        poll.tick().await;
        if let Ok(response) = client.get(format!("{base_url}/")).send().await
            && response.status().is_success()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "router startup deadline exceeded"
        );
    }
}

fn assert_public_headers(private_markers: &PrivateMarkers, headers: &HeaderMap, context: &str) {
    for (name, value) in headers {
        private_markers.assert_absent(context, name.as_str().as_bytes());
        private_markers.assert_absent(
            &format!("{context} public header {}", name.as_str()),
            value.as_bytes(),
        );
    }
}

fn assert_no_internal_brand(context: &str, value: &[u8]) {
    assert!(
        !String::from_utf8_lossy(value)
            .to_ascii_lowercase()
            .contains(PROVIDER_ID),
        "{context} exposed the internal provider brand"
    );
}

fn redacted_log(path: &Path, secret: &str) -> String {
    let bytes = fs::read(path)
        .unwrap_or_else(|error| format!("failed to read sidecar log: {error}").into());
    redacted_text(&bytes, secret)
}

fn redacted_text(bytes: &[u8], secret: &str) -> String {
    String::from_utf8_lossy(bytes).replace(secret, "[REDACTED]")
}

fn validate_proxy_token_shape(secret: &str) {
    let mut segments = secret.split('.');
    let workspace = segments.next().unwrap_or_default();
    let workspace_secret = segments.next().unwrap_or_default();
    assert!(
        segments.next().is_none()
            && valid_proxy_token_segment(workspace, "wk-")
            && valid_proxy_token_segment(workspace_secret, "ws-"),
        "{MODAL_KEY_ENV} must be one dot-joined wk-....ws-... proxy token"
    );
}

fn valid_proxy_token_segment(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    })
}

fn required_billing_review_id() -> Uuid {
    let value = std::env::var(BILLING_REVIEW_ID_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            panic!(
                "{BILLING_REVIEW_ID_ENV} is required after reviewing current provider billing; use a fresh UUID for every run"
            )
        });
    let review_id = Uuid::parse_str(&value)
        .unwrap_or_else(|_| panic!("{BILLING_REVIEW_ID_ENV} must be a fresh UUID"));
    assert!(
        !review_id.is_nil(),
        "{BILLING_REVIEW_ID_ENV} must not be the nil UUID"
    );
    review_id
}

fn required_billing_state_dir() -> PathBuf {
    let value = std::env::var_os(BILLING_STATE_DIR_ENV)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "{BILLING_STATE_DIR_ENV} is required and must name an absolute, durable private directory outside the repository and system temporary directory"
            )
        });
    let directory = PathBuf::from(value);
    assert!(
        directory.is_absolute(),
        "{BILLING_STATE_DIR_ENV} must be an absolute path"
    );
    assert!(
        !directory.starts_with(std::env::temp_dir()),
        "{BILLING_STATE_DIR_ENV} must not use the system temporary directory"
    );
    assert!(
        !directory.starts_with(env!("CARGO_MANIFEST_DIR")),
        "{BILLING_STATE_DIR_ENV} must remain outside the repository"
    );
    directory
}

fn claim_billing_review(
    directory: &Path,
    review_id: Uuid,
    reserved_gross_cost: Decimal,
) -> Decimal {
    assert!(
        reserved_gross_cost > Decimal::ZERO,
        "a billing review must reserve positive gross provider cost"
    );
    ensure_private_directory(directory);
    let marker = directory.join(format!("{}.claimed", review_id.simple()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&marker).unwrap_or_else(|_| {
            panic!(
                "{BILLING_REVIEW_ID_ENV} was already consumed or could not be claimed; review current provider billing and supply a fresh UUID before rerun"
            )
        });
    writeln!(file, "{reserved_gross_cost}")
        .expect("the billing review reservation must be durably writable");
    file.sync_all()
        .expect("the billing review reservation must be flushed before provider access");
    assert_private_file_permissions(&marker);
    // The marker intentionally survives test cleanup so a failed/timeout run cannot
    // be retried without a new billing review acknowledgement.

    let cumulative_reserved = fs::read_dir(directory)
        .expect("the billing state directory must remain readable")
        .filter_map(|entry| {
            let entry = entry.expect("billing state entries must remain readable");
            (entry.path().extension().and_then(|value| value.to_str()) == Some("claimed"))
                .then_some(entry.path())
        })
        .map(|path| {
            assert_private_file_permissions(&path);
            let value = fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("billing reservation {} is unreadable", path.display()));
            let reservation = value.trim().parse::<Decimal>().unwrap_or_else(|_| {
                panic!(
                    "billing reservation {} is invalid; reconcile provider billing before continuing",
                    path.display()
                )
            });
            assert!(
                reservation > Decimal::ZERO,
                "billing reservation {} must be positive",
                path.display()
            );
            reservation
        })
        .try_fold(Decimal::ZERO, |total, reservation| {
            total.checked_add(reservation)
        })
        .expect("cumulative billing reservations overflowed Decimal");
    let approved: Decimal = MAX_APPROVED_SPEND_USD.parse().unwrap();
    assert!(
        cumulative_reserved <= approved,
        "cumulative reserved Modal beta spend ${cumulative_reserved} exceeds the approved ${approved}; no provider request was sent, and the new reservation remains charged until billing is reconciled"
    );
    cumulative_reserved
}

fn create_private_directory(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder
            .create(path)
            .expect("private directory creation failed");
    }
    #[cfg(not(unix))]
    fs::create_dir(path).expect("private directory creation failed");
    assert_private_directory_permissions(path);
}

fn ensure_private_directory(path: &Path) {
    if !path.exists() {
        create_private_directory(path);
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("private directory permissions must be enforceable");
    }
    assert_private_directory_permissions(path);
}

fn create_private_file(path: &Path) -> File {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).expect("private file creation failed");
    assert_private_file_permissions(path);
    file
}

#[cfg(unix)]
fn assert_private_directory_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o700,
        "private test directories must use mode 0700"
    );
}

#[cfg(not(unix))]
fn assert_private_directory_permissions(_path: &Path) {}

#[cfg(unix)]
fn assert_private_file_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600,
        "private test files must use mode 0600"
    );
}

#[cfg(not(unix))]
fn assert_private_file_permissions(_path: &Path) {}

fn required_secret_env(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("{name} is required for the paid Modal beta contract gate"))
}

fn required_database_url() -> String {
    std::env::var(DATABASE_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            panic!(
                "{DATABASE_URL_ENV} is required and must point to a disposable PostgreSQL database"
            )
        })
}

fn required_decimal_env(name: &str) -> Decimal {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("{name} is required for the paid Modal beta contract gate"))
        .parse()
        .unwrap_or_else(|_| panic!("{name} must be a decimal USD amount"))
}

fn sidecar_binary() -> PathBuf {
    std::env::var_os("SEREN_TEST_SIDECAR_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("sidecar/bin/agentgateway"))
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn billing_review_reservations_are_cumulative_and_fail_closed() {
        let directory =
            std::env::temp_dir().join(format!("seren-router-modal-budget-{}", Uuid::new_v4()));
        let first = claim_billing_review(&directory, Uuid::new_v4(), "3.00".parse().unwrap());
        assert_eq!(first, "3.00".parse().unwrap());
        let second = claim_billing_review(&directory, Uuid::new_v4(), "1.50".parse().unwrap());
        assert_eq!(second, "4.50".parse().unwrap());

        let overflow = std::panic::catch_unwind(|| {
            claim_billing_review(&directory, Uuid::new_v4(), "0.51".parse().unwrap())
        });
        assert!(overflow.is_err());
        assert_eq!(
            fs::read_dir(&directory).unwrap().count(),
            3,
            "the failed reservation must remain charged"
        );

        fs::remove_dir_all(directory).unwrap();
    }
}
