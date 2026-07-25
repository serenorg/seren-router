// ABOUTME: Exercises the production sidecar-config init command as a real process.
// ABOUTME: Proves bootstrap rendering stays independent of runtime and secret settings.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use seren_router::registry::Registry;
use seren_router::sidecar_config::{SidecarConfigOptions, compile};
use uuid::Uuid;

struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn create() -> Self {
        let path =
            std::env::temp_dir().join(format!("seren-router-deployment-cli-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn render_command_needs_only_registry_and_output_path() {
    let directory = TempDirectory::create();
    let registry_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sidecar_config_registry.yaml");
    let output_path = directory.path.join("agentgateway.yaml");
    let output = Command::new(env!("CARGO_BIN_EXE_seren-router"))
        .arg("render-sidecar-config")
        .arg(&output_path)
        .env("SEREN_ROUTER_REGISTRY_PATH", &registry_path)
        .env_remove("DATABASE_URL")
        .env_remove("SEREN_ROUTER_GATEWAY_KEY")
        .env_remove("SEREN_TEST_KEY_SLOW")
        .env_remove("SEREN_TEST_KEY_FAST")
        .current_dir(&directory.path)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "renderer failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let registry: Registry = serde_yaml::from_slice(&fs::read(registry_path).unwrap()).unwrap();
    let expected = compile(&registry, SidecarConfigOptions::default()).unwrap();
    assert_eq!(fs::read(output_path).unwrap(), expected);
}

#[test]
fn render_command_fails_closed_on_missing_output_or_invalid_registry() {
    let directory = TempDirectory::create();
    let missing_output = Command::new(env!("CARGO_BIN_EXE_seren-router"))
        .arg("render-sidecar-config")
        .current_dir(&directory.path)
        .output()
        .unwrap();
    assert!(!missing_output.status.success());
    assert!(
        String::from_utf8_lossy(&missing_output.stderr)
            .contains("render-sidecar-config requires exactly one output path")
    );

    let invalid_registry = directory.path.join("invalid.yaml");
    let target = directory.path.join("agentgateway.yaml");
    fs::write(&invalid_registry, b"providers: [").unwrap();
    let invalid = Command::new(env!("CARGO_BIN_EXE_seren-router"))
        .arg("render-sidecar-config")
        .arg(&target)
        .env("SEREN_ROUTER_REGISTRY_PATH", invalid_registry)
        .current_dir(&directory.path)
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(!target.exists());
}
