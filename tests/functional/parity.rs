// ABOUTME: Compares direct OpenRouter completions with the real router and sidecar.
// ABOUTME: Enforces schema, cost, reliability, latency, cleanup, and spend gates.

use std::collections::{BTreeSet, HashSet};
use std::fs::{self, File};
use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use axum::Router;
use reqwest::{Client, StatusCode};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use seren_router::catalog::Catalog;
use seren_router::config::RoutingConfig;
use seren_router::db;
use seren_router::gateway_auth::GatewayAuth;
use seren_router::ledger::Ledger;
use seren_router::policy::measurements::MeasurementStore;
use seren_router::pricing::{ModelPrices, PriceTable};
use seren_router::proxy::ProxyState;
use seren_router::registry::Registry;
use seren_router::routes;
use seren_router::sidecar_config::{SidecarConfigOptions, compile};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, timeout};
use uuid::Uuid;

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const OPENROUTER_KEY_ENV: &str = "SEREN_ROUTER_KEY_OPENROUTER";
const BUDGET_ENV: &str = "SEREN_PARITY_MAX_SPEND_USD";
const MODEL_ENV: &str = "SEREN_PARITY_MODEL";
const DEFAULT_MODEL: &str = "meta-llama/llama-3.3-70b-instruct";
const GATEWAY_KEY: &str = "openrouter-parity-gateway-key";
const MAX_APPROVED_SPEND_USD: &str = "5";
const MAX_COMPLETION_TOKENS: u64 = 1;
const CONSERVATIVE_PROMPT_TOKENS: u64 = 512;
const SOAK_REQUESTS_PER_PATH: usize = 100;
const STARTUP_DEADLINE: Duration = Duration::from_secs(15);
const REQUEST_DEADLINE: Duration = Duration::from_secs(120);
const MAX_P95_ADDED_LATENCY: Duration = Duration::from_millis(50);
static PAID_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Settings {
    openrouter_key: String,
    budget: Decimal,
    model: String,
}

impl Settings {
    fn load(planned_calls: usize) -> Self {
        let openrouter_key = required_secret_env(OPENROUTER_KEY_ENV);
        let budget = required_decimal_env(BUDGET_ENV);
        let maximum: Decimal = MAX_APPROVED_SPEND_USD.parse().unwrap();
        assert!(
            budget > Decimal::ZERO && budget <= maximum,
            "{BUDGET_ENV} must be greater than zero and at most {MAX_APPROVED_SPEND_USD}"
        );
        let model = std::env::var(MODEL_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
        let registry = production_registry();
        let prices = PriceTable::from_registry(&registry)
            .unwrap()
            .get("openrouter", &model)
            .unwrap_or_else(|| panic!("{MODEL_ENV} must name an enabled OpenRouter registry model"))
            .clone();
        let estimated_cost = estimated_maximum_cost(planned_calls, &prices);
        assert!(
            estimated_cost <= budget,
            "planned traffic has a conservative estimated cost of ${estimated_cost}, above the ${budget} budget"
        );

        Self {
            openrouter_key,
            budget,
            model,
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
            values.len()
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
        let directory =
            std::env::temp_dir().join(format!("seren-router-openrouter-{}", Uuid::new_v4()));
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

struct ParityHarness {
    client: Client,
    router_url: String,
    sidecar: OwnedChild,
    router_shutdown: Option<oneshot::Sender<()>>,
    router_task: Option<JoinHandle<std::io::Result<()>>>,
    runtime_ports: [u16; 5],
    _artifacts: Artifacts,
}

impl ParityHarness {
    async fn start(openrouter_key: &str) -> Self {
        let registry = production_registry();
        assert_only_openrouter_is_enabled(&registry);
        let ports = Ports::allocate();
        let runtime_ports = ports.owned_runtime_ports();
        let artifacts = Artifacts::create();
        let options = SidecarConfigOptions {
            llm_port: ports.llm,
            admin_addr: Some(loopback(ports.admin)),
            stats_addr: Some(loopback(ports.stats)),
            readiness_addr: loopback(ports.readiness),
        };
        fs::write(&artifacts.config, compile(&registry, options).unwrap()).unwrap();

        let log = File::create(&artifacts.sidecar_log).unwrap();
        let binary = sidecar_binary();
        let child = Command::new(&binary)
            .arg("-f")
            .arg(&artifacts.config)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap_or_else(|error| panic!("failed to start {}: {error}", binary.display()));
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
            openrouter_key,
        )
        .await;

        ports.router_listener.set_nonblocking(true).unwrap();
        let router_listener = tokio::net::TcpListener::from_std(ports.router_listener).unwrap();
        let database_url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL is required for OpenRouter parity tests");
        let pool = db::connect(&database_url)
            .await
            .expect("parity test database must be reachable");
        sqlx::migrate!()
            .run(&pool)
            .await
            .expect("parity test database migrations must succeed");
        let app = router_app(
            &format!("http://127.0.0.1:{}", ports.llm),
            &registry,
            Ledger::new(pool),
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
            router_url: format!("{router_base_url}/api/v1/chat/completions"),
            sidecar,
            router_shutdown: Some(router_shutdown),
            router_task: Some(router_task),
            runtime_ports,
            _artifacts: artifacts,
        }
    }

    async fn direct(&self, settings: &Settings, stream: bool) -> CompletionResponse {
        send_completion(
            &self.client,
            OPENROUTER_URL,
            &settings.openrouter_key,
            &settings.model,
            stream,
        )
        .await
    }

    async fn routed(&self, settings: &Settings, stream: bool) -> CompletionResponse {
        send_completion(
            &self.client,
            &self.router_url,
            GATEWAY_KEY,
            &settings.model,
            stream,
        )
        .await
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

impl Drop for ParityHarness {
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
    content_type: Option<String>,
    body: Vec<u8>,
    elapsed: Duration,
}

impl CompletionResponse {
    fn assert_success(&self, expected_content_type: &str) {
        assert_eq!(
            self.status,
            StatusCode::OK,
            "completion failed with body: {}",
            String::from_utf8_lossy(&self.body)
        );
        assert!(
            self.content_type
                .as_deref()
                .is_some_and(|value| value.starts_with(expected_content_type)),
            "unexpected content type: {:?}",
            self.content_type
        );
    }
}

struct ParsedStream {
    events: Vec<Value>,
    done_count: usize,
    usage_cost: Decimal,
}

#[tokio::test]
#[ignore = "spends real OpenRouter credits"]
async fn openrouter_response_parity() {
    let _paid_test_guard = PAID_TEST_LOCK.lock().await;
    let settings = Settings::load(2);
    let harness = ParityHarness::start(&settings.openrouter_key).await;

    let direct = harness.direct(&settings, false).await;
    direct.assert_success("application/json");
    let direct_body: Value = serde_json::from_slice(&direct.body).unwrap();
    let direct_cost = usage_cost(&direct_body);
    assert_spend_within_budget(direct_cost, settings.budget);

    let routed = harness.routed(&settings, false).await;
    routed.assert_success("application/json");
    let routed_body: Value = serde_json::from_slice(&routed.body).unwrap();
    assert_eq!(
        key_schema(&direct_body),
        key_schema(&routed_body),
        "non-streaming response key schema drifted"
    );
    let routed_cost = usage_cost(&routed_body);
    assert_cost_parity(direct_cost, routed_cost);
    assert_spend_within_budget(direct_cost + routed_cost, settings.budget);

    println!(
        "openrouter_response_parity model={} direct_ms={:.3} routed_ms={:.3} direct_cost={} routed_cost={}",
        settings.model,
        milliseconds(direct.elapsed),
        milliseconds(routed.elapsed),
        direct_cost,
        routed_cost,
    );
    harness.shutdown_and_assert_clean().await;
}

#[tokio::test]
#[ignore = "spends real OpenRouter credits"]
async fn openrouter_streaming_parity() {
    let _paid_test_guard = PAID_TEST_LOCK.lock().await;
    let settings = Settings::load(2);
    let harness = ParityHarness::start(&settings.openrouter_key).await;

    let direct = harness.direct(&settings, true).await;
    direct.assert_success("text/event-stream");
    let direct_stream = parse_stream(&direct.body);
    assert_spend_within_budget(direct_stream.usage_cost, settings.budget);

    let routed = harness.routed(&settings, true).await;
    routed.assert_success("text/event-stream");
    let routed_stream = parse_stream(&routed.body);
    assert_eq!(direct_stream.done_count, 1);
    assert_eq!(routed_stream.done_count, 1);
    assert_eq!(
        stream_schema(&direct_stream),
        stream_schema(&routed_stream),
        "streaming event key schema drifted"
    );
    assert_cost_parity(direct_stream.usage_cost, routed_stream.usage_cost);
    assert_spend_within_budget(
        direct_stream.usage_cost + routed_stream.usage_cost,
        settings.budget,
    );

    println!(
        "openrouter_streaming_parity model={} direct_ms={:.3} routed_ms={:.3} direct_cost={} routed_cost={}",
        settings.model,
        milliseconds(direct.elapsed),
        milliseconds(routed.elapsed),
        direct_stream.usage_cost,
        routed_stream.usage_cost,
    );
    harness.shutdown_and_assert_clean().await;
}

#[tokio::test]
#[ignore = "spends real OpenRouter credits"]
async fn openrouter_streaming_soak() {
    let _paid_test_guard = PAID_TEST_LOCK.lock().await;
    let settings = Settings::load(SOAK_REQUESTS_PER_PATH * 2);
    let harness = ParityHarness::start(&settings.openrouter_key).await;
    let mut direct_latencies = Vec::with_capacity(SOAK_REQUESTS_PER_PATH);
    let mut routed_latencies = Vec::with_capacity(SOAK_REQUESTS_PER_PATH);
    let mut direct_cost = Decimal::ZERO;
    let mut routed_cost = Decimal::ZERO;

    for sequence in 1..=SOAK_REQUESTS_PER_PATH {
        let direct = harness.direct(&settings, true).await;
        direct.assert_success("text/event-stream");
        let direct_stream = parse_stream(&direct.body);
        assert_eq!(
            direct_stream.done_count, 1,
            "direct request {sequence} omitted [DONE]"
        );
        direct_latencies.push(direct.elapsed);
        direct_cost += direct_stream.usage_cost;
        assert_spend_within_budget(direct_cost + routed_cost, settings.budget);

        let routed = harness.routed(&settings, true).await;
        routed.assert_success("text/event-stream");
        let routed_stream = parse_stream(&routed.body);
        assert_eq!(
            routed_stream.done_count, 1,
            "routed request {sequence} omitted [DONE]"
        );
        routed_latencies.push(routed.elapsed);
        routed_cost += routed_stream.usage_cost;
        assert_spend_within_budget(direct_cost + routed_cost, settings.budget);
    }

    let direct_p95 = percentile_95(&direct_latencies);
    let routed_p95 = percentile_95(&routed_latencies);
    let added_p95 = routed_p95.saturating_sub(direct_p95);
    assert!(
        added_p95 < MAX_P95_ADDED_LATENCY,
        "p95 router-added latency was {:.3} ms, at or above the 50 ms gate",
        milliseconds(added_p95)
    );

    println!(
        "openrouter_streaming_soak model={} requests_per_path={} failures=0 direct_p95_ms={:.3} routed_p95_ms={:.3} added_p95_ms={:.3} direct_cost={} routed_cost={} total_cost={}",
        settings.model,
        SOAK_REQUESTS_PER_PATH,
        milliseconds(direct_p95),
        milliseconds(routed_p95),
        milliseconds(added_p95),
        direct_cost,
        routed_cost,
        direct_cost + routed_cost,
    );
    harness.shutdown_and_assert_clean().await;
}

async fn send_completion(
    client: &Client,
    url: &str,
    bearer: &str,
    model: &str,
    stream: bool,
) -> CompletionResponse {
    let mut body = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": "Reply with exactly one lowercase word: pong"
        }],
        "temperature": 0,
        "max_tokens": MAX_COMPLETION_TOKENS,
        "stream": stream
    });
    if stream {
        body["stream_options"] = json!({"include_usage": true});
    }

    let started_at = Instant::now();
    let response = client
        .post(url)
        .bearer_auth(bearer)
        .json(&body)
        .send()
        .await
        .unwrap_or_else(|error| panic!("completion transport failed for {url}: {error}"));
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response
        .bytes()
        .await
        .unwrap_or_else(|error| panic!("completion body failed for {url}: {error}"))
        .to_vec();

    CompletionResponse {
        status,
        content_type,
        body,
        elapsed: started_at.elapsed(),
    }
}

fn parse_stream(body: &[u8]) -> ParsedStream {
    let normalized = String::from_utf8(body.to_vec())
        .expect("stream must be UTF-8")
        .replace("\r\n", "\n");
    let mut events: Vec<Value> = Vec::new();
    let mut done_count = 0;
    let mut seen_done = false;

    for frame in normalized.split("\n\n").filter(|frame| !frame.is_empty()) {
        let data_lines: Vec<_> = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect();
        if data_lines.is_empty() {
            assert!(
                frame.lines().all(|line| line.starts_with(':')),
                "SSE frame without data must contain only comments"
            );
            continue;
        }
        assert_eq!(
            data_lines.len(),
            1,
            "every SSE frame must have one data line"
        );
        let data = data_lines[0];
        if data == "[DONE]" {
            done_count += 1;
            seen_done = true;
            continue;
        }
        assert!(!seen_done, "JSON event arrived after [DONE]");
        events.push(
            serde_json::from_str(data)
                .unwrap_or_else(|error| panic!("stream event was not JSON: {error}")),
        );
    }

    let usage_events: Vec<_> = events
        .iter()
        .filter(|event| {
            event["choices"]
                .as_array()
                .is_some_and(|choices| choices.is_empty())
                && event["usage"]["cost"].is_number()
        })
        .collect();
    assert_eq!(
        usage_events.len(),
        1,
        "stream must have exactly one empty-choices usage-cost event"
    );
    let usage_cost = usage_cost(usage_events[0]);

    ParsedStream {
        events,
        done_count,
        usage_cost,
    }
}

fn key_schema(value: &Value) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    collect_key_paths(value, "$", &mut paths);
    paths
}

fn collect_key_paths(value: &Value, path: &str, paths: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = format!("{path}.{key}");
                paths.insert(child_path.clone());
                collect_key_paths(child, &child_path, paths);
            }
        }
        Value::Array(values) => {
            let child_path = format!("{path}[]");
            for child in values {
                collect_key_paths(child, &child_path, paths);
            }
        }
        _ => {}
    }
}

fn stream_schema(stream: &ParsedStream) -> BTreeSet<BTreeSet<String>> {
    stream.events.iter().map(key_schema).collect()
}

fn usage_cost(value: &Value) -> Decimal {
    value["usage"]["cost"]
        .to_string()
        .parse()
        .expect("usage.cost must be a decimal JSON number")
}

fn assert_cost_parity(direct: Decimal, routed: Decimal) {
    assert!(direct > Decimal::ZERO, "direct usage.cost must be positive");
    let relative_delta = (direct - routed).abs() / direct;
    assert!(
        relative_delta <= Decimal::new(1, 2),
        "usage.cost drifted by {relative_delta}, above the 1% gate (direct={direct}, routed={routed})"
    );
}

fn assert_spend_within_budget(observed: Decimal, budget: Decimal) {
    assert!(
        observed <= budget,
        "observed OpenRouter spend ${observed} exceeded the ${budget} budget"
    );
}

fn estimated_maximum_cost(planned_calls: usize, prices: &ModelPrices) -> Decimal {
    let per_call = (Decimal::from(CONSERVATIVE_PROMPT_TOKENS) * prices.input_price_per_mtok
        + Decimal::from(MAX_COMPLETION_TOKENS) * prices.output_price_per_mtok)
        / Decimal::from(1_000_000_u64);
    Decimal::from(planned_calls as u64) * per_call
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

fn production_registry() -> Registry {
    serde_yaml::from_str(include_str!("../../registry/providers.yaml")).unwrap()
}

fn assert_only_openrouter_is_enabled(registry: &Registry) {
    let enabled: Vec<_> = registry
        .providers
        .iter()
        .filter(|provider| provider.enabled)
        .collect();
    assert_eq!(
        enabled.len(),
        1,
        "parity requires exactly one enabled provider"
    );
    assert_eq!(enabled[0].id, "openrouter");
}

fn router_app(sidecar_url: &str, registry: &Registry, ledger: Ledger) -> Router {
    let auth = GatewayAuth::new(GATEWAY_KEY.as_bytes());
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

fn redacted_log(path: &Path, secret: &str) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| format!("failed to read sidecar log: {error}"))
        .replace(secret, "[REDACTED]")
}

fn required_secret_env(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("{name} is required for paid OpenRouter parity tests"))
}

fn required_decimal_env(name: &str) -> Decimal {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("{name} is required for paid OpenRouter parity tests"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_parser_requires_and_extracts_one_terminal_cost_event() {
        let mut input = b": keepalive\n\n".to_vec();
        input.extend_from_slice(include_bytes!("../golden/streaming_chat_cost.sse"));
        let stream = parse_stream(&input);

        assert_eq!(stream.done_count, 1);
        assert_eq!(stream.events.len(), 4);
        assert_eq!(stream.usage_cost, "0.0000088000".parse().unwrap());
        assert!(stream_schema(&stream).len() >= 2);
    }

    #[test]
    fn key_schema_ignores_values_and_object_order() {
        let left = json!({
            "id": "one",
            "usage": {"cost": 1, "details": [{"cached": true}]}
        });
        let right = json!({
            "usage": {"details": [{"cached": false}], "cost": 999},
            "id": "two"
        });

        assert_eq!(key_schema(&left), key_schema(&right));
    }

    #[test]
    fn percentile_uses_the_nearest_rank_definition() {
        let samples: Vec<_> = (1..=100).map(Duration::from_millis).collect();

        assert_eq!(percentile_95(&samples), Duration::from_millis(95));
    }

    #[test]
    fn full_soak_estimate_for_the_default_model_is_below_ten_cents() {
        let registry = production_registry();
        let prices = PriceTable::from_registry(&registry)
            .unwrap()
            .get("openrouter", DEFAULT_MODEL)
            .unwrap()
            .clone();

        assert!(
            estimated_maximum_cost(SOAK_REQUESTS_PER_PATH * 2, &prices) < "0.10".parse().unwrap()
        );
    }
}
