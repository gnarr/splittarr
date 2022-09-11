pub mod settings {
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
        lidarr: Lidarr,
        shnsplit: Shnsplit,
    }

    impl Settings {
        // pub fn new() -> Result<Self, ConfigError> {
        pub fn new() -> Config {
            let dirs = dirs();
            let config_dir = dirs.config_dir();
            let config_file = config_dir.join("config.toml");
            let config = Config::builder()
                .add_source(File::with_name(config_file.to_str().unwrap()))
                .add_source(Environment::with_prefix("splittarr"))
                .set_default("shnsplit.path", "shnsplit")
                .unwrap()
                .build()
                .unwrap();
            config
        }
    }
}
