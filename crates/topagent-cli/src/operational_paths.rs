use anyhow::Result;
use std::path::PathBuf;

use crate::config::TELEGRAM_SERVICE_UNIT_NAME;

const TELEGRAM_SERVICE_ENV_DIR: &str = "topagent/services";
const TELEGRAM_SERVICE_ENV_FILE: &str = "topagent-telegram.env";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServicePaths {
    pub unit_dir: PathBuf,
    pub unit_path: PathBuf,
    pub env_dir: PathBuf,
    pub env_path: PathBuf,
}

pub(crate) fn resolve_config_home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Could not determine your config directory. Set XDG_CONFIG_HOME or HOME first."
            )
        })?;
    Ok(home.join(".config"))
}

pub(crate) fn service_paths() -> Result<ServicePaths> {
    Ok(service_paths_from_config_home(resolve_config_home()?))
}

pub(crate) fn managed_service_env_path() -> Result<PathBuf> {
    Ok(service_paths()?.env_path)
}

fn service_paths_from_config_home(config_home: PathBuf) -> ServicePaths {
    ServicePaths {
        unit_dir: config_home.join("systemd").join("user"),
        unit_path: config_home
            .join("systemd")
            .join("user")
            .join(TELEGRAM_SERVICE_UNIT_NAME),
        env_dir: config_home.join("topagent").join("services"),
        env_path: config_home
            .join(TELEGRAM_SERVICE_ENV_DIR)
            .join(TELEGRAM_SERVICE_ENV_FILE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_paths_from_config_home_keeps_unit_and_env_paths_aligned() {
        let config_home = PathBuf::from("/tmp/topagent-config");
        let paths = service_paths_from_config_home(config_home.clone());

        assert_eq!(paths.unit_dir, config_home.join("systemd").join("user"));
        assert_eq!(
            paths.unit_path,
            config_home
                .join("systemd")
                .join("user")
                .join(TELEGRAM_SERVICE_UNIT_NAME)
        );
        assert_eq!(paths.env_dir, config_home.join("topagent").join("services"));
        assert_eq!(
            paths.env_path,
            config_home
                .join("topagent")
                .join("services")
                .join("topagent-telegram.env")
        );
    }
}
