use crate::globals::dirs;
use config::{Config, Environment, File};
use serde::Deserialize;

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
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct Settings {
    debug: bool,
    data_dir: String,
    check_frequency_seconds: u64,
    lidarr: Lidarr,
    shnsplit: Shnsplit,
}

pub fn get_settings() -> Config {
    let dirs = dirs();
    let config_dir = dirs.config_dir();
    let config_file = config_dir.join("config.toml");

    let config = Config::builder()
        .add_source(File::from(config_file).required(false))
        .add_source(Environment::with_prefix("splittarr"))
        .set_default("data_dir", dirs.data_dir().to_str())
        .unwrap()
        .set_default("shnsplit.path", "shnsplit")
        .unwrap()
        .set_default("check_frequency_seconds", 60)
        .unwrap()
        .build()
        .expect("ERROR");

    config
}
