// ABOUTME: Validates compiled fixture YAML with the real pinned agentgateway binary.
// ABOUTME: Keeps the schema-drift gate ignored locally and explicit in CI.

use std::fs::{File, remove_file};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use seren_router::registry::Registry;
use seren_router::sidecar_config::{SidecarConfigOptions, compile};
use uuid::Uuid;

struct TempConfig {
    path: PathBuf,
}

impl TempConfig {
    fn create(bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "seren-router-sidecar-config-{}.yaml",
            Uuid::new_v4()
        ));
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(bytes).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        let _ = remove_file(&self.path);
    }
}

#[test]
#[ignore = "needs sidecar binary"]
fn compiled_config_validates_with_pinned_sidecar() {
    let registry: Registry =
        serde_yaml::from_str(include_str!("fixtures/sidecar_config_registry.yaml")).unwrap();
    let yaml = compile(&registry, SidecarConfigOptions::default()).unwrap();
    let config = TempConfig::create(&yaml);
    let binary = Path::new(env!("CARGO_MANIFEST_DIR")).join("sidecar/bin/agentgateway");

    let output = Command::new(&binary)
        .arg("-f")
        .arg(config.path())
        .arg("--validate-only")
        .env("SEREN_ROUTER_KEY_SLOW", "fixture-only-slow-key")
        .env("SEREN_ROUTER_KEY_FAST", "fixture-only-fast-key")
        .output()
        .unwrap_or_else(|error| panic!("failed to execute {}: {error}", binary.display()));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "agentgateway validation failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Configuration is valid!"),
        "validation success marker missing\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
#[ignore = "needs sidecar binary"]
fn production_registry_validates_with_pinned_sidecar() {
    let registry: Registry =
        serde_yaml::from_str(include_str!("../registry/providers.yaml")).unwrap();
    let yaml = compile(&registry, SidecarConfigOptions::default()).unwrap();
    let config = TempConfig::create(&yaml);
    let binary = Path::new(env!("CARGO_MANIFEST_DIR")).join("sidecar/bin/agentgateway");

    let output = Command::new(&binary)
        .arg("-f")
        .arg(config.path())
        .arg("--validate-only")
        .env("SEREN_ROUTER_KEY_OPENROUTER", "fixture-only-openrouter-key")
        .output()
        .unwrap_or_else(|error| panic!("failed to execute {}: {error}", binary.display()));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "agentgateway validation failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Configuration is valid!"),
        "validation success marker missing\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
