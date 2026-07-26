// ABOUTME: Owns the real agentgateway, Axum router, ports, logs, and temp config.
// ABOUTME: Exercises chat, SSE, authentication, and failover without network mocks.

use std::collections::HashSet;
use std::fs::{self, File};
use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use axum::Router;
use reqwest::{Client, Response, StatusCode, Url};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use seren_router::attribution::SERVED_PROVIDER_HEADER;
use seren_router::catalog::Catalog;
use seren_router::config::RoutingConfig;
use seren_router::db;
use seren_router::gateway_auth::GatewayAuth;
use seren_router::ledger::Ledger;
use seren_router::policy::measurements::{MeasurementStore, Observation};
use seren_router::pricing::{PriceTable, Usage, cost_usd};
use seren_router::proxy::ProxyState;
use seren_router::registry::{ModelMapping, Provider, Registry};
use seren_router::sidecar_config::{SidecarConfigOptions, compile};
use seren_router::{routes, server};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, timeout};
use uuid::Uuid;

const GATEWAY_KEY: &str = "functional-gateway-key";
const VIRTUAL_MODEL: &str = "functional-model";
const LOCAL_MODEL: &str = "local/functional-model";
const DEFAULT_UPSTREAM_URL: &str = "http://127.0.0.1:1234/v1";
const DEFAULT_MODEL: &str = "gemma-3-1b-it-glm-4.7-flash-heretic-uncensored-thinking_gguf";
const STARTUP_DEADLINE: Duration = Duration::from_secs(15);

struct Ports {
    llm: u16,
    readiness: u16,
    admin: u16,
    stats: u16,
    dead_upstream: u16,
    router: u16,
    router_listener: StdTcpListener,
}

impl Ports {
    fn allocate() -> Self {
        let reservations: Vec<_> = (0..6)
            .map(|_| StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap())
            .collect();
        let values: Vec<_> = reservations
            .iter()
            .map(|listener| listener.local_addr().unwrap().port())
            .collect();
        assert_eq!(
            values.iter().copied().collect::<HashSet<_>>().len(),
            values.len()
        );

        let router_listener = reservations.last().unwrap().try_clone().unwrap();
        drop(reservations);

        Self {
            llm: values[0],
            readiness: values[1],
            admin: values[2],
            stats: values[3],
            dead_upstream: values[4],
            router: values[5],
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
        let directory =
            std::env::temp_dir().join(format!("seren-router-functional-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
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

struct FunctionalHarness {
    client: Client,
    router_base_url: String,
    sidecar_base_url: String,
    sidecar: OwnedChild,
    router_shutdown: Option<oneshot::Sender<()>>,
    router_task: Option<JoinHandle<std::io::Result<()>>>,
    runtime_ports: [u16; 5],
    artifacts: Artifacts,
    measurements: MeasurementStore,
}

#[derive(Clone, Copy)]
struct StreamTiming {
    headers: Duration,
    first_chunk: Duration,
    complete: Duration,
}

impl FunctionalHarness {
    async fn start() -> Self {
        let ports = Ports::allocate();
        let runtime_ports = ports.owned_runtime_ports();
        let artifacts = Artifacts::create();
        let upstream_url = env_or_default("SEREN_TEST_UPSTREAM_URL", DEFAULT_UPSTREAM_URL);
        let model = env_or_default("SEREN_TEST_MODEL", DEFAULT_MODEL);
        let registry = functional_registry(&upstream_url, &model, ports.dead_upstream);
        let options = SidecarConfigOptions {
            llm_port: ports.llm,
            admin_addr: Some(loopback(ports.admin)),
            stats_addr: Some(loopback(ports.stats)),
            readiness_addr: loopback(ports.readiness),
            enable_ipv6: false,
        };
        fs::write(&artifacts.config, compile(&registry, options).unwrap()).unwrap();

        let log = File::create(&artifacts.sidecar_log).unwrap();
        let binary = sidecar_binary();
        let child = Command::new(&binary)
            .arg("-f")
            .arg(&artifacts.config)
            .env("SEREN_TEST_KEY_DEAD", "functional-fixture-only")
            .env("SEREN_TEST_KEY_LOCAL", "functional-fixture-only")
            .env("SEREN_TEST_KEY_EXPENSIVE", "functional-fixture-only")
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap_or_else(|error| panic!("failed to start {}: {error}", binary.display()));
        let mut sidecar = OwnedChild { child };
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(250))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        wait_for_sidecar(
            &client,
            &mut sidecar,
            ports.readiness,
            &artifacts.sidecar_log,
        )
        .await;

        ports.router_listener.set_nonblocking(true).unwrap();
        let router_listener = tokio::net::TcpListener::from_std(ports.router_listener).unwrap();
        let sidecar_url = format!("http://127.0.0.1:{}", ports.llm);
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL is required for functional tests");
        let pool = db::connect(&database_url)
            .await
            .expect("functional test database must be reachable");
        sqlx::migrate!()
            .run(&pool)
            .await
            .expect("functional test database migrations must succeed");
        let measurements = MeasurementStore::default();
        let ledger = Ledger::new(pool.clone());
        let app = router_app(&sidecar_url, &registry, ledger, measurements.clone()).merge(
            server::health_router(
                pool,
                Url::parse(&format!(
                    "http://127.0.0.1:{}/healthz/ready",
                    ports.readiness
                ))
                .unwrap(),
            )
            .unwrap(),
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
            sidecar_base_url: sidecar_url,
            sidecar,
            router_shutdown: Some(router_shutdown),
            router_task: Some(router_task),
            runtime_ports,
            artifacts,
            measurements,
        }
    }

    async fn chat(&self, model: &str, stream: bool) -> Response {
        self.chat_with_sort(model, stream, None).await
    }

    async fn chat_with_sort(&self, model: &str, stream: bool, sort: Option<&str>) -> Response {
        let mut body = json!({
            "model": model,
            "messages": [{"role": "user", "content": "Reply only with the word pong."}],
            "temperature": 0,
            "max_tokens": 16,
            "stream": stream
        });
        if stream {
            body["stream_options"] = json!({"include_usage": true});
        }
        if let Some(sort) = sort {
            body["provider"] = json!({"sort": sort});
        }

        self.client
            .post(format!("{}/api/v1/chat/completions", self.router_base_url))
            .bearer_auth(GATEWAY_KEY)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    fn seed_measurement(
        &self,
        provider_id: &str,
        completion_tokens: u64,
        stream_duration: Duration,
        time_to_first_token: Duration,
    ) {
        self.measurements
            .observe(
                provider_id,
                VIRTUAL_MODEL,
                Observation {
                    completion_tokens,
                    stream_duration,
                    time_to_first_token,
                },
            )
            .unwrap();
    }

    async fn unauthenticated_chat(&self) -> Response {
        self.client
            .post(format!("{}/api/v1/chat/completions", self.router_base_url))
            .json(&json!({
                "model": LOCAL_MODEL,
                "messages": [{"role": "user", "content": "pong"}],
                "max_tokens": 4
            }))
            .send()
            .await
            .unwrap()
    }

    async fn raw_completion(&self, path: &str, body: impl Into<reqwest::Body>) -> Response {
        self.client
            .post(format!("{}{path}", self.router_base_url))
            .bearer_auth(GATEWAY_KEY)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .unwrap()
    }

    async fn models(&self) -> Response {
        self.client
            .get(format!("{}/api/v1/models", self.router_base_url))
            .bearer_auth(GATEWAY_KEY)
            .send()
            .await
            .unwrap()
    }

    async fn unauthenticated_models(&self) -> Response {
        self.client
            .get(format!("{}/api/v1/models", self.router_base_url))
            .send()
            .await
            .unwrap()
    }

    async fn auth_key(&self) -> Response {
        self.authenticated_get("/api/v1/auth/key").await
    }

    async fn credits(&self) -> Response {
        self.authenticated_get("/api/v1/credits").await
    }

    async fn model_endpoints(&self, model: &str) -> Response {
        self.authenticated_get(&format!("/api/v1/models/{model}/endpoints"))
            .await
    }

    async fn unauthenticated_auth_key(&self) -> Response {
        self.unauthenticated_get("/api/v1/auth/key").await
    }

    async fn unauthenticated_credits(&self) -> Response {
        self.unauthenticated_get("/api/v1/credits").await
    }

    async fn unauthenticated_model_endpoints(&self) -> Response {
        self.unauthenticated_get(&format!("/api/v1/models/{VIRTUAL_MODEL}/endpoints"))
            .await
    }

    async fn authenticated_get(&self, path: &str) -> Response {
        self.client
            .get(format!("{}{path}", self.router_base_url))
            .bearer_auth(GATEWAY_KEY)
            .send()
            .await
            .unwrap()
    }

    async fn unauthenticated_get(&self, path: &str) -> Response {
        self.client
            .get(format!("{}{path}", self.router_base_url))
            .send()
            .await
            .unwrap()
    }

    async fn generation(&self, id: &str) -> Response {
        let mut url = Url::parse(&format!("{}/api/v1/generation", self.router_base_url)).unwrap();
        url.query_pairs_mut().append_pair("id", id);
        self.client
            .get(url)
            .bearer_auth(GATEWAY_KEY)
            .send()
            .await
            .unwrap()
    }

    async fn unauthenticated_generation(&self) -> Response {
        self.client
            .get(format!(
                "{}/api/v1/generation?id=unknown",
                self.router_base_url
            ))
            .send()
            .await
            .unwrap()
    }

    async fn livez(&self) -> Response {
        self.unauthenticated_get("/livez").await
    }

    async fn readyz(&self) -> Response {
        self.unauthenticated_get("/readyz").await
    }

    fn terminate_sidecar(&mut self) {
        self.sidecar.terminate();
    }

    async fn sidecar_chat(&self, model: &str) -> Response {
        self.client
            .post(format!("{}/v1/chat/completions", self.sidecar_base_url))
            .json(&json!({
                "model": model,
                "messages": [{"role": "user", "content": "Reply only with the word pong."}],
                "temperature": 0,
                "max_tokens": 16
            }))
            .send()
            .await
            .unwrap()
    }

    async fn timed_stream(&self, url: String, model: &str, bearer: Option<&str>) -> StreamTiming {
        let body = json!({
            "model": model,
            "messages": [{"role": "user", "content": "Reply only with the word pong."}],
            "temperature": 0,
            "max_tokens": 1,
            "stream": true,
            "stream_options": {"include_usage": true}
        });
        let started_at = Instant::now();
        let mut request = self.client.post(url).json(&body);
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        let mut response = request.send().await.unwrap();
        let headers = started_at.elapsed();
        assert_eq!(response.status(), StatusCode::OK);
        let mut first_chunk = None;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.unwrap() {
            first_chunk.get_or_insert_with(|| started_at.elapsed());
            body.extend_from_slice(&chunk);
        }
        assert!(
            String::from_utf8(body)
                .unwrap()
                .replace("\r\n", "\n")
                .ends_with("data: [DONE]\n\n")
        );

        StreamTiming {
            headers,
            first_chunk: first_chunk.expect("stream must contain at least one chunk"),
            complete: started_at.elapsed(),
        }
    }

    fn sidecar_request_count(&self) -> usize {
        fs::read_to_string(&self.artifacts.sidecar_log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("request gateway="))
            .count()
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

impl Drop for FunctionalHarness {
    fn drop(&mut self) {
        if let Some(shutdown) = self.router_shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.router_task.take() {
            task.abort();
        }
    }
}

fn router_app(
    sidecar_url: &str,
    registry: &Registry,
    ledger: Ledger,
    measurements: MeasurementStore,
) -> Router {
    let auth = GatewayAuth::new(GATEWAY_KEY.as_bytes());
    let catalog = Catalog::from_registry(registry);
    let proxy = ProxyState::new(
        sidecar_url,
        PriceTable::from_registry(registry).unwrap(),
        ledger.clone(),
        registry,
        RoutingConfig::new("2.0".parse().unwrap(), 0.1, Decimal::ONE, 100).unwrap(),
        measurements,
    )
    .unwrap();
    routes::public_router().merge(routes::protected_router(auth, proxy, ledger, catalog))
}

fn functional_registry(upstream_url: &str, model: &str, dead_port: u16) -> Registry {
    Registry {
        providers: vec![
            functional_provider(
                "dead",
                format!("http://127.0.0.1:{dead_port}/v1"),
                "SEREN_TEST_KEY_DEAD",
                0,
                model,
                "9.00",
                "9.00",
            ),
            functional_provider(
                "local",
                upstream_url.to_owned(),
                "SEREN_TEST_KEY_LOCAL",
                1,
                model,
                "0.40",
                "0.80",
            ),
            functional_provider(
                "expensive",
                upstream_url.to_owned(),
                "SEREN_TEST_KEY_EXPENSIVE",
                2,
                model,
                "2.00",
                "4.00",
            ),
        ],
    }
}

fn functional_provider(
    id: &str,
    base_url: String,
    secret_env: &str,
    priority: u8,
    model: &str,
    input_price_per_mtok: &str,
    output_price_per_mtok: &str,
) -> Provider {
    Provider {
        id: id.to_owned(),
        display_name: format!("Functional {id}"),
        base_url,
        secret_env: secret_env.to_owned(),
        enabled: true,
        priority,
        models: vec![ModelMapping {
            slug: VIRTUAL_MODEL.to_owned(),
            name: "Functional Model".to_owned(),
            context_length: 131_072,
            provider_model_id: model.to_owned(),
            input_price_per_mtok: input_price_per_mtok.parse().unwrap(),
            output_price_per_mtok: output_price_per_mtok.parse().unwrap(),
        }],
    }
}

async fn wait_for_sidecar(
    client: &Client,
    child: &mut OwnedChild,
    readiness_port: u16,
    log_path: &Path,
) {
    let url = format!("http://127.0.0.1:{readiness_port}/healthz/ready");
    let deadline = Instant::now() + STARTUP_DEADLINE;
    let mut poll = tokio::time::interval(Duration::from_millis(50));
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        poll.tick().await;
        if let Ok(response) = client.get(&url).send().await
            && response.status().is_success()
        {
            return;
        }
        if let Some(status) = child.try_wait() {
            panic!(
                "agentgateway exited during startup with {status}\n{}",
                fs::read_to_string(log_path).unwrap_or_default()
            );
        }
        assert!(
            Instant::now() < deadline,
            "agentgateway readiness deadline exceeded\n{}",
            fs::read_to_string(log_path).unwrap_or_default()
        );
    }
}

async fn wait_for_router(client: &Client, base_url: &str) {
    let deadline = Instant::now() + STARTUP_DEADLINE;
    let mut poll = tokio::time::interval(Duration::from_millis(25));
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

fn env_or_default(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn sidecar_binary() -> PathBuf {
    std::env::var_os("SEREN_TEST_SIDECAR_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("sidecar/bin/agentgateway"))
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

#[tokio::test]
#[ignore = "functional"]
async fn functional_composite_readiness_tracks_real_sidecar_lifecycle() {
    let mut harness = FunctionalHarness::start().await;

    let ready = harness.readyz().await;
    assert_eq!(ready.status(), StatusCode::OK);
    assert_eq!(
        ready.json::<Value>().await.unwrap(),
        json!({"status": "ok"})
    );

    harness.terminate_sidecar();
    let deadline = Instant::now() + Duration::from_secs(5);
    let unavailable = loop {
        let response = harness.readyz().await;
        if response.status() == StatusCode::SERVICE_UNAVAILABLE {
            break response;
        }
        assert!(
            Instant::now() < deadline,
            "router remained ready after AgentGateway stopped"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        unavailable.json::<Value>().await.unwrap(),
        json!({"status": "unavailable", "reason": "sidecar"})
    );
    assert_eq!(harness.livez().await.status(), StatusCode::OK);

    harness.shutdown_and_assert_clean().await;
}

#[tokio::test]
#[ignore = "functional"]
async fn functional_chat_completion() {
    let harness = FunctionalHarness::start().await;
    let models = harness.models().await;
    assert_eq!(models.status(), StatusCode::OK);
    assert_eq!(
        models.json::<Value>().await.unwrap(),
        json!({
            "data": [{
                "id": VIRTUAL_MODEL,
                "name": "Functional Model",
                "context_length": 131072,
                "pricing": {
                    "prompt": "0.0000004",
                    "completion": "0.0000008"
                }
            }],
            "links": {"next": null},
            "total_count": 1
        })
    );

    let response = harness.chat(VIRTUAL_MODEL, false).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert!(
        body["choices"][0]["message"]["content"]
            .as_str()
            .is_some_and(|content| !content.is_empty())
    );
    assert!(body["usage"]["prompt_tokens"].as_u64().unwrap_or(0) > 0);
    assert_local_usage_cost(&body);
    let generation_id = body["id"]
        .as_str()
        .expect("completion must include a provider response id");
    let generation = wait_for_generation(&harness, generation_id).await;
    assert_generation_matches(&generation, &body);
    let measurement = harness
        .measurements
        .get("local", VIRTUAL_MODEL)
        .expect("non-streaming completion must update provider measurements");
    assert!(measurement.throughput_tokens_per_second > 0.0);
    assert!(measurement.time_to_first_token_seconds >= 0.0);

    harness.shutdown_and_assert_clean().await;
}

#[tokio::test]
#[ignore = "functional"]
async fn functional_compatibility_metadata() {
    let harness = FunctionalHarness::start().await;
    let sidecar_requests = harness.sidecar_request_count();

    let auth_key = harness.auth_key().await;
    assert_eq!(auth_key.status(), StatusCode::OK);
    assert_eq!(
        auth_key.json::<Value>().await.unwrap(),
        json!({"data": {"label": "seren-router", "limit": null}})
    );

    let credits = harness.credits().await;
    assert_eq!(credits.status(), StatusCode::OK);
    assert_eq!(
        credits.json::<Value>().await.unwrap(),
        json!({"data": {"total_credits": 0, "total_usage": 0}})
    );

    let endpoints = harness.model_endpoints(VIRTUAL_MODEL).await;
    assert_eq!(endpoints.status(), StatusCode::OK);
    assert_eq!(
        endpoints.json::<Value>().await.unwrap(),
        json!({
            "data": {
                "id": VIRTUAL_MODEL,
                "name": "Functional Model",
                "endpoints": [
                    {
                        "name": "Functional dead: Functional Model",
                        "model_id": VIRTUAL_MODEL,
                        "model_name": "Functional Model",
                        "context_length": 131072,
                        "pricing": {
                            "prompt": "0.000009",
                            "completion": "0.000009"
                        },
                        "provider_name": "Functional dead",
                        "tag": "dead"
                    },
                    {
                        "name": "Functional local: Functional Model",
                        "model_id": VIRTUAL_MODEL,
                        "model_name": "Functional Model",
                        "context_length": 131072,
                        "pricing": {
                            "prompt": "0.0000004",
                            "completion": "0.0000008"
                        },
                        "provider_name": "Functional local",
                        "tag": "local"
                    },
                    {
                        "name": "Functional expensive: Functional Model",
                        "model_id": VIRTUAL_MODEL,
                        "model_name": "Functional Model",
                        "context_length": 131072,
                        "pricing": {
                            "prompt": "0.000002",
                            "completion": "0.000004"
                        },
                        "provider_name": "Functional expensive",
                        "tag": "expensive"
                    }
                ]
            }
        })
    );

    let unknown = harness.model_endpoints("unknown%2Fmodel").await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        unknown.json::<Value>().await.unwrap(),
        json!({"error": {"code": 404, "message": "Not Found"}})
    );
    assert_eq!(harness.sidecar_request_count(), sidecar_requests);

    harness.shutdown_and_assert_clean().await;
}

#[tokio::test]
#[ignore = "functional"]
async fn functional_streaming() {
    let harness = FunctionalHarness::start().await;
    let response = harness.chat(VIRTUAL_MODEL, true).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .unwrap(),
        "text/event-stream"
    );
    let body = response.text().await.unwrap();
    let data: Vec<_> = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect();
    assert!(data.iter().any(|event| {
        serde_json::from_str::<Value>(event)
            .ok()
            .and_then(|value| {
                value["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_owned)
            })
            .is_some_and(|content| !content.is_empty())
    }));
    let usage_event = data
        .iter()
        .filter_map(|event| serde_json::from_str::<Value>(event).ok())
        .find(|value| {
            value["choices"]
                .as_array()
                .is_some_and(|choices| choices.is_empty())
                && value["usage"]["prompt_tokens"]
                    .as_u64()
                    .is_some_and(|tokens| tokens > 0)
        })
        .expect("stream must contain a terminal usage event");
    assert_local_usage_cost(&usage_event);
    assert_eq!(data.last(), Some(&"[DONE]"));
    let generation_id = usage_event["id"]
        .as_str()
        .expect("terminal usage event must include a provider response id");
    let generation = wait_for_generation(&harness, generation_id).await;
    assert_generation_matches(&generation, &usage_event);
    let measurement = harness
        .measurements
        .get("local", VIRTUAL_MODEL)
        .expect("streaming completion must update provider measurements");
    assert!(measurement.throughput_tokens_per_second > 0.0);
    assert!(measurement.time_to_first_token_seconds >= 0.0);

    harness.shutdown_and_assert_clean().await;
}

#[tokio::test]
#[ignore = "requires real LM Studio, stock agentgateway, and PostgreSQL"]
async fn functional_streaming_component_latency() {
    const WARMUPS: usize = 5;
    const SAMPLES: usize = 50;
    const MAX_P95_ADDED_LATENCY: Duration = Duration::from_millis(50);

    let harness = FunctionalHarness::start().await;
    let upstream_url = env_or_default("SEREN_TEST_UPSTREAM_URL", DEFAULT_UPSTREAM_URL);
    let upstream_model = env_or_default("SEREN_TEST_MODEL", DEFAULT_MODEL);
    let paths = [
        (
            format!("{upstream_url}/chat/completions"),
            upstream_model.as_str(),
            None,
        ),
        (
            format!("{}/v1/chat/completions", harness.sidecar_base_url),
            LOCAL_MODEL,
            None,
        ),
        (
            format!("{}/api/v1/chat/completions", harness.router_base_url),
            VIRTUAL_MODEL,
            Some(GATEWAY_KEY),
        ),
    ];

    for _ in 0..WARMUPS {
        for (url, model, bearer) in &paths {
            harness
                .timed_stream(url.clone(), model, bearer.as_deref())
                .await;
        }
    }

    let mut samples = [Vec::new(), Vec::new(), Vec::new()];
    for sequence in 0..SAMPLES {
        let order = match sequence % 3 {
            0 => [0, 1, 2],
            1 => [1, 2, 0],
            _ => [2, 0, 1],
        };
        for index in order {
            let (url, model, bearer) = &paths[index];
            samples[index].push(
                harness
                    .timed_stream(url.clone(), model, bearer.as_deref())
                    .await,
            );
        }
    }

    let summaries = samples.map(|timings| StreamTiming {
        headers: percentile_95(
            &timings
                .iter()
                .map(|timing| timing.headers)
                .collect::<Vec<_>>(),
        ),
        first_chunk: percentile_95(
            &timings
                .iter()
                .map(|timing| timing.first_chunk)
                .collect::<Vec<_>>(),
        ),
        complete: percentile_95(
            &timings
                .iter()
                .map(|timing| timing.complete)
                .collect::<Vec<_>>(),
        ),
    });
    for (label, timing) in ["direct", "sidecar", "router"].into_iter().zip(&summaries) {
        println!(
            "stream_component_latency path={label} samples={SAMPLES} headers_p95_ms={:.3} first_chunk_p95_ms={:.3} complete_p95_ms={:.3}",
            milliseconds(timing.headers),
            milliseconds(timing.first_chunk),
            milliseconds(timing.complete),
        );
    }
    for (segment, direct, sidecar, router) in [
        (
            "headers",
            summaries[0].headers,
            summaries[1].headers,
            summaries[2].headers,
        ),
        (
            "first_chunk",
            summaries[0].first_chunk,
            summaries[1].first_chunk,
            summaries[2].first_chunk,
        ),
        (
            "complete",
            summaries[0].complete,
            summaries[1].complete,
            summaries[2].complete,
        ),
    ] {
        let sidecar_added = sidecar.saturating_sub(direct);
        let router_added = router.saturating_sub(direct);
        let app_added = router.saturating_sub(sidecar);
        println!(
            "stream_component_added_latency segment={segment} sidecar_p95_ms={:.3} app_p95_ms={:.3} total_p95_ms={:.3}",
            milliseconds(sidecar_added),
            milliseconds(app_added),
            milliseconds(router_added),
        );
        assert!(
            router_added < MAX_P95_ADDED_LATENCY,
            "local {segment} p95 router-added latency was {:.3} ms, at or above the 50 ms gate",
            milliseconds(router_added),
        );
    }

    harness.shutdown_and_assert_clean().await;
}

#[tokio::test]
#[ignore = "functional"]
async fn functional_auth_rejected() {
    let harness = FunctionalHarness::start().await;
    assert_eq!(harness.sidecar_request_count(), 0);

    let response = harness.unauthenticated_chat().await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"error": {"message": "unauthorized"}})
    );
    assert_eq!(harness.sidecar_request_count(), 0);
    assert_eq!(
        harness.unauthenticated_generation().await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        harness.unauthenticated_models().await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        harness.unauthenticated_auth_key().await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        harness.unauthenticated_credits().await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        harness.unauthenticated_model_endpoints().await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(harness.sidecar_request_count(), 0);

    harness.shutdown_and_assert_clean().await;
}

#[tokio::test]
#[ignore = "functional"]
async fn functional_failover() {
    let harness = FunctionalHarness::start().await;

    let sidecar_first = harness.sidecar_chat(VIRTUAL_MODEL).await;
    assert_eq!(
        sidecar_first.status(),
        StatusCode::OK,
        "sidecar retry policy must save the first virtual-model request"
    );
    assert_eq!(
        sidecar_first.headers().get(SERVED_PROVIDER_HEADER).unwrap(),
        "local"
    );

    let router_first = harness
        .chat_with_sort(VIRTUAL_MODEL, false, Some("throughput"))
        .await;
    assert_eq!(
        router_first.status(),
        StatusCode::OK,
        "router safety-net retry must save the first selected dead route"
    );
    assert_eq!(router_first.headers().get(SERVED_PROVIDER_HEADER), None);
    let body: Value = router_first.json().await.unwrap();
    assert!(
        body["choices"][0]["message"]["content"]
            .as_str()
            .is_some_and(|content| !content.is_empty())
    );
    assert_local_usage_cost(&body);
    let generation = wait_for_generation(
        &harness,
        body["id"]
            .as_str()
            .expect("failover response must include an id"),
    )
    .await;
    assert_generation_matches(&generation, &body);
    assert!(
        harness.measurements.get("local", VIRTUAL_MODEL).is_some(),
        "fallback measurements must be attributed to the actual served provider"
    );

    harness.shutdown_and_assert_clean().await;
}

#[tokio::test]
#[ignore = "functional"]
async fn functional_invalid_requests_stay_local() {
    let harness = FunctionalHarness::start().await;
    assert_eq!(harness.sidecar_request_count(), 0);

    let malformed = harness
        .raw_completion("/api/v1/chat/completions", "{")
        .await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        malformed.json::<Value>().await.unwrap(),
        json!({"error": {"message": "request body must be valid JSON"}})
    );

    let invalid_model = harness
        .raw_completion("/api/v1/completions", r#"{"prompt":"hello"}"#)
        .await;
    assert_eq!(invalid_model.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_model.json::<Value>().await.unwrap(),
        json!({"error": {"message": "model must be a string"}})
    );

    let invalid_sort = harness
        .raw_completion(
            "/api/v1/chat/completions",
            r#"{"model":"functional-model","provider":{"sort":"quality"}}"#,
        )
        .await;
    assert_eq!(invalid_sort.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_sort.json::<Value>().await.unwrap(),
        json!({
            "error": {
                "message": "provider.sort must be one of: price, throughput, latency"
            }
        })
    );

    let unknown = harness
        .raw_completion(
            "/api/v1/completions",
            r#"{"model":"unknown/model","prompt":"hello"}"#,
        )
        .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        unknown.json::<Value>().await.unwrap(),
        json!({"error": {"code": 404, "message": "model not found"}})
    );
    assert_eq!(
        harness.sidecar_request_count(),
        0,
        "locally rejected completion requests must never reach the sidecar"
    );

    harness.shutdown_and_assert_clean().await;
}

#[tokio::test]
#[ignore = "functional"]
async fn functional_sort_modes() {
    let harness = FunctionalHarness::start().await;

    let price = harness
        .chat_with_sort(VIRTUAL_MODEL, false, Some("price"))
        .await;
    assert_eq!(price.status(), StatusCode::OK);
    let price_body: Value = price.json().await.unwrap();
    let price_generation = wait_for_generation(
        &harness,
        price_body["id"]
            .as_str()
            .expect("price response must include an id"),
    )
    .await;
    assert_eq!(price_generation["data"]["provider_name"], "local");

    harness.seed_measurement(
        "local",
        10,
        Duration::from_secs(1),
        Duration::from_millis(500),
    );
    harness.seed_measurement(
        "expensive",
        1_000,
        Duration::from_secs(1),
        Duration::from_millis(10),
    );
    let default = harness.chat(VIRTUAL_MODEL, false).await;
    assert_eq!(default.status(), StatusCode::OK);
    let default_body: Value = default.json().await.unwrap();
    let default_generation = wait_for_generation(
        &harness,
        default_body["id"]
            .as_str()
            .expect("default response must include an id"),
    )
    .await;
    assert_eq!(
        default_generation["data"]["provider_name"], "local",
        "the much faster expensive provider must remain above the default ceiling"
    );

    harness.shutdown_and_assert_clean().await;
}

fn assert_local_usage_cost(body: &Value) {
    let usage = Usage {
        prompt_tokens: body["usage"]["prompt_tokens"].as_u64().unwrap(),
        completion_tokens: body["usage"]["completion_tokens"].as_u64().unwrap(),
    };
    let expected = cost_usd(
        &seren_router::pricing::ModelPrices {
            input_price_per_mtok: "0.40".parse().unwrap(),
            output_price_per_mtok: "0.80".parse().unwrap(),
        },
        &usage,
    );
    let actual: Decimal = body["usage"]["cost"]
        .as_number()
        .expect("usage.cost must be a JSON number")
        .to_string()
        .parse()
        .unwrap();

    assert_eq!(actual, expected);
}

fn assert_generation_matches(generation: &Value, response: &Value) {
    assert_eq!(generation["data"]["id"], response["id"]);
    assert_eq!(generation["data"]["model"], VIRTUAL_MODEL);
    assert_eq!(generation["data"]["provider_name"], "local");
    assert_eq!(
        generation["data"]["tokens_prompt"],
        response["usage"]["prompt_tokens"]
    );
    assert_eq!(
        generation["data"]["tokens_completion"],
        response["usage"]["completion_tokens"]
    );
    assert_eq!(
        generation["data"]["total_cost"].as_number().unwrap(),
        response["usage"]["cost"].as_number().unwrap()
    );
    assert!(generation["data"]["created_at"].as_str().is_some());
    assert!(generation["data"]["latency"].as_i64().unwrap() >= 0);
}

async fn wait_for_generation(harness: &FunctionalHarness, id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let response = harness.generation(id).await;
        if response.status() == StatusCode::OK {
            return response.json().await.unwrap();
        }
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "unexpected generation lookup status"
        );
        assert!(
            Instant::now() < deadline,
            "generation {id} was not persisted before the lookup deadline"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn percentile_95(samples: &[Duration]) -> Duration {
    assert!(!samples.is_empty());
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = (sorted.len() * 95).div_ceil(100);
    sorted[rank - 1]
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
