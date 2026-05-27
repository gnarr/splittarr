use std::path::PathBuf;

use clap::Parser;
use config::{Config, ConfigError, Environment, File};
use directories::ProjectDirs;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(author, version, about)]
pub struct Cli {
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Cue {
    pub strict: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Lidarr {
    pub url: String,
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Shnsplit {
    pub path: PathBuf,
    pub overwrite: bool,
    pub format: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub data_dir: PathBuf,
    pub check_frequency_seconds: u64,
    pub cue: Cue,
    pub lidarr: Lidarr,
    pub shnsplit: Shnsplit,
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("could not determine an application data directory")]
    MissingProjectDirs,
    #[error(transparent)]
    Config(#[from] ConfigError),
}

impl Settings {
    pub fn load(config_file: Option<PathBuf>) -> Result<Self, SettingsError> {
        let dirs = ProjectDirs::from("org", "Gnarr", "Splittarr")
            .ok_or(SettingsError::MissingProjectDirs)?;
        Self::load_with_paths(
            config_file,
            dirs.data_dir().to_path_buf(),
            Some(dirs.config_dir().join("config.toml")),
        )
    }

    fn load_with_paths(
        config_file: Option<PathBuf>,
        default_data_dir: PathBuf,
        project_config_file: Option<PathBuf>,
    ) -> Result<Self, SettingsError> {
        let mut builder = Config::builder()
            .set_default("data_dir", default_data_dir.to_string_lossy().to_string())?
            .set_default("check_frequency_seconds", 60)?
            .set_default("cue.strict", false)?
            .set_default("shnsplit.path", "shnsplit")?
            .set_default("shnsplit.overwrite", true)?
            .set_default("shnsplit.format", "%p - %a - %n - %t")?
            .add_source(File::with_name("config.toml").required(false))
            .add_source(File::with_name("/config/config.toml").required(false));

        if let Some(path) = project_config_file {
            builder = builder.add_source(File::from(path).required(false));
        }

        if let Some(path) = config_file {
            builder = builder.add_source(File::from(path).required(true));
        }

        let config = builder
            .add_source(
                Environment::with_prefix("SPLITTARR")
                    .prefix_separator("_")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?;

        Ok(config.try_deserialize::<Settings>()?)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use tempfile::tempdir;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn explicit_config_overrides_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_test_env();
        let tmp = tempdir().unwrap();
        let config_path = tmp.path().join("splittarr.toml");
        fs::write(
            &config_path,
            r#"
check_frequency_seconds = 5
data_dir = "/tmp/splittarr-data"

[lidarr]
url = "http://lidarr"
api_key = "secret"

[cue]
strict = true

[shnsplit]
path = "/usr/bin/shnsplit"
overwrite = false
format = "%n - %t"
"#,
        )
        .unwrap();

        let settings =
            Settings::load_with_paths(Some(config_path), tmp.path().join("default"), None).unwrap();

        assert_eq!(settings.check_frequency_seconds, 5);
        assert_eq!(settings.data_dir, PathBuf::from("/tmp/splittarr-data"));
        assert!(settings.cue.strict);
        assert_eq!(settings.lidarr.url, "http://lidarr");
        assert_eq!(settings.shnsplit.path, PathBuf::from("/usr/bin/shnsplit"));
        assert!(!settings.shnsplit.overwrite);
    }

    #[test]
    fn environment_overrides_config_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_test_env();
        let tmp = tempdir().unwrap();
        let config_path = tmp.path().join("splittarr.toml");
        fs::write(
            &config_path,
            r#"
check_frequency_seconds = 5

[lidarr]
url = "http://from-file"
api_key = "file-secret"
"#,
        )
        .unwrap();

        std::env::set_var("SPLITTARR_CHECK_FREQUENCY_SECONDS", "9");
        std::env::set_var("SPLITTARR_LIDARR__URL", "http://from-env");

        let settings =
            Settings::load_with_paths(Some(config_path), tmp.path().join("default"), None).unwrap();

        std::env::remove_var("SPLITTARR_CHECK_FREQUENCY_SECONDS");
        std::env::remove_var("SPLITTARR_LIDARR__URL");

        assert_eq!(settings.check_frequency_seconds, 9);
        assert_eq!(settings.lidarr.url, "http://from-env");
        assert_eq!(settings.lidarr.api_key, "file-secret");
    }

    fn clear_test_env() {
        std::env::remove_var("SPLITTARR_CHECK_FREQUENCY_SECONDS");
        std::env::remove_var("SPLITTARR_LIDARR__URL");
    }
}
