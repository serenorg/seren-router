// ABOUTME: Renders the validated provider registry into an atomic sidecar config file.
// ABOUTME: Supports the production init-container boundary without resolving secrets.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;
use uuid::Uuid;

use crate::registry::Registry;
use crate::sidecar_config::{SidecarConfigError, SidecarConfigOptions, compile};

#[derive(Debug, Error)]
pub enum RenderSidecarConfigError {
    #[error("sidecar config output path must name a file")]
    InvalidOutputPath,
    #[error("failed to read provider registry {path}: {source}")]
    ReadRegistry {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse provider registry {path}: {source}")]
    ParseRegistry {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    #[error("failed to compile sidecar configuration: {0}")]
    Compile(#[from] SidecarConfigError),
    #[error("failed to create temporary sidecar configuration beside {path}: {source}")]
    CreateOutput {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write temporary sidecar configuration beside {path}: {source}")]
    WriteOutput {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to replace sidecar configuration {path}: {source}")]
    PersistOutput {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub fn render_sidecar_config(
    registry_path: &Path,
    output_path: &Path,
) -> Result<(), RenderSidecarConfigError> {
    if output_path.as_os_str().is_empty() || output_path.file_name().is_none() {
        return Err(RenderSidecarConfigError::InvalidOutputPath);
    }
    let parent = output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let registry_bytes =
        fs::read(registry_path).map_err(|source| RenderSidecarConfigError::ReadRegistry {
            path: registry_path.to_owned(),
            source,
        })?;
    let registry: Registry = serde_yaml::from_slice(&registry_bytes).map_err(|source| {
        RenderSidecarConfigError::ParseRegistry {
            path: registry_path.to_owned(),
            source,
        }
    })?;
    let rendered = compile(&registry, SidecarConfigOptions::default())?;
    let temporary_path = parent.join(format!(".seren-router-sidecar-{}.tmp", Uuid::new_v4()));
    let mut temporary = TemporaryOutput::create(&temporary_path, output_path)?;
    temporary.write_all(&rendered, output_path)?;
    temporary.persist(output_path)
}

struct TemporaryOutput {
    path: PathBuf,
    file: Option<File>,
}

impl TemporaryOutput {
    fn create(path: &Path, output_path: &Path) -> Result<Self, RenderSidecarConfigError> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|source| RenderSidecarConfigError::CreateOutput {
                path: output_path.to_owned(),
                source,
            })?;
        Ok(Self {
            path: path.to_owned(),
            file: Some(file),
        })
    }

    fn write_all(
        &mut self,
        rendered: &[u8],
        output_path: &Path,
    ) -> Result<(), RenderSidecarConfigError> {
        let file = self
            .file
            .as_mut()
            .expect("temporary sidecar config file is present before persistence");
        file.write_all(rendered)
            .and_then(|()| file.sync_all())
            .map_err(|source| RenderSidecarConfigError::WriteOutput {
                path: output_path.to_owned(),
                source,
            })
    }

    fn persist(mut self, output_path: &Path) -> Result<(), RenderSidecarConfigError> {
        drop(self.file.take());
        fs::rename(&self.path, output_path).map_err(|source| {
            RenderSidecarConfigError::PersistOutput {
                path: output_path.to_owned(),
                source,
            }
        })
    }
}

impl Drop for TemporaryOutput {
    fn drop(&mut self) {
        if self.file.take().is_some() || self.path.exists() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDirectory {
        path: PathBuf,
    }

    impl TempDirectory {
        fn create() -> Self {
            let path = std::env::temp_dir()
                .join(format!("seren-router-deployment-test-{}", Uuid::new_v4()));
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
    fn renderer_atomically_replaces_output_with_compiled_environment_references() {
        let directory = TempDirectory::create();
        let registry_path = directory.path.join("providers.yaml");
        let output_path = directory.path.join("agentgateway.yaml");
        let fixture = include_bytes!("../tests/fixtures/sidecar_config_registry.yaml");
        fs::write(&registry_path, fixture).unwrap();
        fs::write(&output_path, b"stale").unwrap();

        render_sidecar_config(&registry_path, &output_path).unwrap();

        let registry: Registry = serde_yaml::from_slice(fixture).unwrap();
        let expected = compile(&registry, SidecarConfigOptions::default()).unwrap();
        let actual = fs::read(&output_path).unwrap();
        assert_eq!(actual, expected);
        assert!(
            actual
                .windows(b"$SEREN_ROUTER_KEY_SLOW".len())
                .any(|window| { window == b"$SEREN_ROUTER_KEY_SLOW" })
        );
        assert_eq!(
            fs::read_dir(&directory.path).unwrap().count(),
            2,
            "temporary output leaked"
        );
    }

    #[test]
    fn invalid_registry_does_not_replace_existing_output() {
        let directory = TempDirectory::create();
        let registry_path = directory.path.join("providers.yaml");
        let output_path = directory.path.join("agentgateway.yaml");
        fs::write(&registry_path, b"not: [valid").unwrap();
        fs::write(&output_path, b"known-good").unwrap();

        assert!(matches!(
            render_sidecar_config(&registry_path, &output_path),
            Err(RenderSidecarConfigError::ParseRegistry { .. })
        ));
        assert_eq!(fs::read(&output_path).unwrap(), b"known-good");
    }
}
