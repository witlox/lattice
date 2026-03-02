//! Configuration file support.
//!
//! Loads user configuration from `~/.config/lattice/config.toml`.
//! CLI arguments take precedence over config file values.

use serde::{Deserialize, Serialize};

/// User configuration loaded from `~/.config/lattice/config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CliConfig {
    /// Server address (e.g., "localhost:50051")
    pub server_addr: Option<String>,
    /// Default tenant
    pub default_tenant: Option<String>,
    /// Default vCluster
    pub default_vcluster: Option<String>,
    /// Output format: table, json, wide
    pub output_format: Option<String>,
}

impl CliConfig {
    /// Load config from the default path (~/.config/lattice/config.toml).
    /// Returns default config if the file doesn't exist.
    pub fn load() -> Self {
        Self::load_from_path(&Self::default_path())
    }

    /// Load config from a specific path.
    pub fn load_from_path(path: &str) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => toml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Default config file path.
    pub fn default_path() -> String {
        if let Some(config_dir) = dirs::config_dir() {
            config_dir
                .join("lattice")
                .join("config.toml")
                .to_string_lossy()
                .to_string()
        } else {
            "~/.config/lattice/config.toml".to_string()
        }
    }

    /// Merge CLI overrides into this config. CLI values take precedence.
    pub fn merge_cli_overrides(
        &self,
        tenant: Option<&str>,
        vcluster: Option<&str>,
        output: Option<&str>,
    ) -> CliConfig {
        CliConfig {
            server_addr: self.server_addr.clone(),
            default_tenant: tenant
                .map(String::from)
                .or_else(|| self.default_tenant.clone()),
            default_vcluster: vcluster
                .map(String::from)
                .or_else(|| self.default_vcluster.clone()),
            output_format: output
                .map(String::from)
                .or_else(|| self.output_format.clone()),
        }
    }

    /// Get the server address, with a fallback default.
    pub fn server_addr(&self) -> &str {
        self.server_addr.as_deref().unwrap_or("localhost:50051")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = CliConfig::default();
        assert!(config.server_addr.is_none());
        assert!(config.default_tenant.is_none());
        assert_eq!(config.server_addr(), "localhost:50051");
    }

    #[test]
    fn parse_toml_config() {
        let toml = r#"
server_addr = "lattice.example.com:50051"
default_tenant = "physics"
default_vcluster = "hpc-backfill"
output_format = "json"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.server_addr(), "lattice.example.com:50051");
        assert_eq!(config.default_tenant.as_deref(), Some("physics"));
        assert_eq!(config.default_vcluster.as_deref(), Some("hpc-backfill"));
        assert_eq!(config.output_format.as_deref(), Some("json"));
    }

    #[test]
    fn cli_overrides_config() {
        let config = CliConfig {
            server_addr: Some("server:50051".into()),
            default_tenant: Some("physics".into()),
            default_vcluster: Some("hpc".into()),
            output_format: Some("table".into()),
        };

        let merged = config.merge_cli_overrides(Some("bio"), None, Some("json"));
        assert_eq!(merged.default_tenant.as_deref(), Some("bio"));
        assert_eq!(merged.default_vcluster.as_deref(), Some("hpc")); // not overridden
        assert_eq!(merged.output_format.as_deref(), Some("json"));
    }

    #[test]
    fn load_nonexistent_file_returns_default() {
        let config = CliConfig::load_from_path("/nonexistent/path/config.toml");
        assert!(config.server_addr.is_none());
    }

    #[test]
    fn default_path_is_reasonable() {
        let path = CliConfig::default_path();
        assert!(path.contains("lattice") && path.contains("config.toml"));
    }
}
