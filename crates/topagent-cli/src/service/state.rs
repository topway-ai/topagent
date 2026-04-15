use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::{
    RuntimeModelSelection, TOPAGENT_WORKSPACE_KEY, resolve_runtime_model_selection,
};
use crate::managed_files::{is_topagent_managed_file, read_managed_env_metadata};
use crate::operational_paths::{ServicePaths, service_paths};

use super::lifecycle::{ensure_systemd_user_available, load_service_status_snapshot};
use super::managed_env::persisted_model_from_env_values;

#[derive(Debug, Clone)]
pub(super) struct ControlPlaneState {
    pub paths: ServicePaths,
    pub env_values: HashMap<String, String>,
    pub setup_installed: bool,
    pub model_selection: RuntimeModelSelection,
    pub service_probe: ServiceProbe,
}

impl ControlPlaneState {
    pub(super) fn workspace(&self) -> Option<&str> {
        self.env_values
            .get(TOPAGENT_WORKSPACE_KEY)
            .map(String::as_str)
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct ServiceStatusSnapshot {
    pub load_state: Option<String>,
    pub unit_file_state: Option<String>,
    pub active_state: Option<String>,
    pub sub_state: Option<String>,
    pub fragment_path: Option<String>,
    pub result: Option<String>,
    pub exec_main_status: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ServiceProbe {
    pub systemd_error: Option<String>,
    pub snapshot_error: Option<String>,
    pub snapshot: Option<ServiceStatusSnapshot>,
    pub service_installed: bool,
}

impl ServiceProbe {
    pub(super) fn unit_path(&self, default_path: &Path) -> PathBuf {
        self.snapshot
            .as_ref()
            .and_then(|status| status.fragment_path.as_ref())
            .map(PathBuf::from)
            .unwrap_or_else(|| default_path.to_path_buf())
    }
}

pub(super) fn load_control_plane_state(
    explicit_model: Option<String>,
) -> Result<ControlPlaneState> {
    let paths = service_paths()?;
    let env_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let config_installed = paths.env_path.exists() && is_topagent_managed_file(&paths.env_path)?;
    let service_probe = load_service_probe(&paths);
    let service_installed = service_probe.service_installed;
    let setup_installed = config_installed || service_installed;
    let model_selection = resolve_runtime_model_selection(
        explicit_model,
        persisted_model_from_env_values(&env_values),
    );

    Ok(ControlPlaneState {
        paths,
        env_values,
        setup_installed,
        model_selection,
        service_probe,
    })
}

pub(super) fn load_service_probe(paths: &ServicePaths) -> ServiceProbe {
    let systemd_available = ensure_systemd_user_available().map_err(|e| e.to_string());
    let snapshot_result = if systemd_available.is_ok() {
        Some(load_service_status_snapshot().map_err(|e| e.to_string()))
    } else {
        None
    };
    let snapshot = snapshot_result
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .cloned();
    let snapshot_error = snapshot_result.and_then(|result| result.err());
    let service_installed = snapshot
        .as_ref()
        .and_then(|status| status.load_state.as_deref())
        .map(|state| state != "not-found")
        .unwrap_or(paths.unit_path.exists());

    ServiceProbe {
        systemd_error: systemd_available.err(),
        snapshot_error,
        snapshot,
        service_installed,
    }
}
