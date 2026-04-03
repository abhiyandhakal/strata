use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct StrataConfig {
    #[serde(default)]
    pub editor: EditorConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct EditorConfig {
    #[serde(default)]
    pub vim_mode: bool,
}

impl StrataConfig {
    pub fn load() -> Result<Self> {
        Self::load_with_env(
            std::env::var("STRATA_CONFIG_PATH").ok(),
            std::env::var("STRATA_VIM_MODE").ok(),
            std::env::var("XDG_CONFIG_HOME").ok(),
            std::env::var("HOME").ok(),
        )
    }

    fn load_with_env(
        config_path_override: Option<String>,
        vim_override: Option<String>,
        xdg_config_home: Option<String>,
        home: Option<String>,
    ) -> Result<Self> {
        let path = config_path(
            config_path_override.as_deref(),
            xdg_config_home.as_deref(),
            home.as_deref(),
        );
        let mut config = if let Some(path) = path {
            load_from_path(&path)?
        } else {
            Self::default()
        };
        config.editor.vim_mode = resolve_vim_mode(config.editor.vim_mode, vim_override.as_deref());
        Ok(config)
    }
}

fn load_from_path(path: &Path) -> Result<StrataConfig> {
    if !path.exists() {
        return Ok(StrataConfig::default());
    }
    let body = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    let config = toml::from_str::<StrataConfig>(&body)
        .with_context(|| format!("failed to parse config {}", path.display()))?;
    Ok(config)
}

fn config_path(
    config_path_override: Option<&str>,
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> Option<PathBuf> {
    if let Some(path) = config_path_override {
        return Some(PathBuf::from(path));
    }
    if let Some(xdg) = xdg_config_home {
        return Some(PathBuf::from(xdg).join("strata").join("config.toml"));
    }
    home.map(|home| {
        PathBuf::from(home)
            .join(".config")
            .join("strata")
            .join("config.toml")
    })
}

fn resolve_vim_mode(config_value: bool, vim_override: Option<&str>) -> bool {
    match vim_override.and_then(parse_bool) {
        Some(value) => value,
        None => config_value,
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn config_uses_default_path_from_home() {
        let path = config_path(None, None, Some("/tmp/home")).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/home/.config/strata/config.toml"));
    }

    #[test]
    fn config_reads_vim_mode_from_toml() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("config.toml");
        fs::write(&path, "[editor]\nvim_mode = true\n").unwrap();

        let config = load_from_path(&path).unwrap();
        assert!(config.editor.vim_mode);
    }

    #[test]
    fn env_override_wins_over_file_setting() {
        let config = StrataConfig::load_with_env(
            None,
            Some("0".to_string()),
            None,
            Some("/tmp/home".to_string()),
        )
        .unwrap();

        assert!(!config.editor.vim_mode);
        assert!(resolve_vim_mode(false, Some("true")));
        assert!(!resolve_vim_mode(true, Some("off")));
    }
}
