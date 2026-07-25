// ABOUTME: Owns the real agentgateway, Axum router, ports, logs, and temp config.
// ABOUTME: Exercises chat, SSE, authentication, and failover without network mocks.

use std::collections::HashSet;
use std::fs::{self, File};
use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use axum::Router;
use reqwest::{Client, Response, StatusCode};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use seren_router::attribution::SERVED_PROVIDER_HEADER;
use seren_router::gateway_auth::GatewayAuth;
use seren_router::pricing::{PriceTable, Usage, cost_usd};
use seren_router::proxy::ProxyState;
use seren_router::registry::{ModelMapping, Provider, Registry};
use seren_router::routes;
use seren_router::sidecar_config::{SidecarConfigOptions, compile};
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
        };
        fs::write(&artifacts.config, compile(&registry, options).unwrap()).unwrap();

        let log = File::create(&artifacts.sidecar_log).unwrap();
        let binary = sidecar_binary();
        let child = Command::new(&binary)
            .arg("-f")
            .arg(&artifacts.config)
            .env("SEREN_TEST_KEY_DEAD", "functional-fixture-only")
            .env("SEREN_TEST_KEY_LOCAL", "functional-fixture-only")
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
        let app = router_app(&sidecar_url, &registry);
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
        }
    }

    async fn chat(&self, model: &str, stream: bool) -> Response {
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

        self.client
            .post(format!("{}/api/v1/chat/completions", self.router_base_url))
            .bearer_auth(GATEWAY_KEY)
            .json(&body)
            .send()
            .await
            .unwrap()
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

fn router_app(sidecar_url: &str, registry: &Registry) -> Router {
    let auth = GatewayAuth::new(GATEWAY_KEY.as_bytes());
    let proxy = ProxyState::new(sidecar_url, PriceTable::from_registry(registry).unwrap()).unwrap();
    routes::public_router().merge(routes::protected_router(auth, proxy))
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
async fn functional_chat_completion() {
    let harness = FunctionalHarness::start().await;
    let response = harness.chat(LOCAL_MODEL, false).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert!(
        body["choices"][0]["message"]["content"]
            .as_str()
            .is_some_and(|content| !content.is_empty())
    );
    assert!(body["usage"]["prompt_tokens"].as_u64().unwrap_or(0) > 0);
    assert_local_usage_cost(&body);

    harness.shutdown_and_assert_clean().await;
}

#[tokio::test]
#[ignore = "functional"]
async fn functional_streaming() {
    let harness = FunctionalHarness::start().await;
    let response = harness.chat(LOCAL_MODEL, true).await;

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
    assert!(data.iter().any(|event| {
        serde_json::from_str::<Value>(event)
            .ok()
            .and_then(|value| value["usage"]["prompt_tokens"].as_u64())
            .is_some_and(|tokens| tokens > 0)
    }));
    assert_eq!(data.last(), Some(&"[DONE]"));

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

    harness.shutdown_and_assert_clean().await;
}

#[tokio::test]
#[ignore = "functional"]
async fn functional_failover() {
    let harness = FunctionalHarness::start().await;

    let first = harness.chat(VIRTUAL_MODEL, false).await;
    assert!(
        first.status().is_success() || first.status().is_server_error(),
        "unexpected first-attempt status: {}",
        first.status()
    );
    let second = harness.chat(VIRTUAL_MODEL, false).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(second.headers().get(SERVED_PROVIDER_HEADER), None);
    let body: Value = second.json().await.unwrap();
    assert!(
        body["choices"][0]["message"]["content"]
            .as_str()
            .is_some_and(|content| !content.is_empty())
    );
    assert_local_usage_cost(&body);

    let attributed = harness.sidecar_chat(VIRTUAL_MODEL).await;
    assert_eq!(attributed.status(), StatusCode::OK);
    assert_eq!(
        attributed.headers().get(SERVED_PROVIDER_HEADER).unwrap(),
        "local"
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
        .expect("non-streaming usage.cost must be a JSON number")
        .to_string()
        .parse()
        .unwrap();

    assert_eq!(actual, expected);
}
