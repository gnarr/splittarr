use crate::globals::dirs;
use config::{Config, ConfigError, Environment, File};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct Cue {
    pub strict: bool,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct Lidarr {
    pub url: String,
    pub api_key: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct Shnsplit {
    pub path: String,
    pub overwrite: bool,
    pub format: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct Settings {
    pub data_dir: String,
    pub check_frequency_seconds: u64,
    pub cue: Cue,
    pub lidarr: Lidarr,
    pub shnsplit: Shnsplit,
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let dirs = dirs();
        let config_dir = dirs.config_dir();
        let config_file = config_dir.join("config.toml");

        let config = Config::builder()
            .add_source(Environment::with_prefix("splittarr"))
            .add_source(File::with_name("config.toml").required(false))
            .add_source(File::with_name("/config/config.toml").required(false))
            .add_source(File::from(config_file).required(false))
            .set_default("data_dir", dirs.data_dir().to_str())
            .unwrap()
            .set_default("check_frequency_seconds", "60")
            .unwrap()
            .set_default("cue.strict", false)
            .unwrap()
            .set_default("shnsplit.path", "shnsplit")
            .unwrap()
            .set_default("shnsplit.overwrite", true)
            .unwrap()
            .set_default("shnsplit.format", "%p - %a - %n - %t")
            .unwrap()
            .build()?;

        config.try_deserialize::<Settings>()
    }
}
