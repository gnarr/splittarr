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
pub struct CueSettings {
    pub strict: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LidarrSettings {
    pub url: String,
    pub api_key: String,
    pub queue_page_size: usize,
    pub queue_max_pages: usize,
    pub manual_import_enabled: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ShnsplitSettings {
    pub path: PathBuf,
    pub overwrite: bool,
    pub format: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ServerSettings {
    pub bind_address: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LoggingSettings {
    pub download_log_enabled: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct GnudbSettings {
    pub disc_lookup_enabled: bool,
    pub server: String,
    pub user_email: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MusicBrainzSettings {
    pub disc_lookup_enabled: bool,
    pub base_url: String,
    pub trust_disc_lookup: bool,
    pub add_missing_release_group_enabled: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub data_dir: PathBuf,
    pub check_frequency_seconds: u64,
    pub server: ServerSettings,
    pub logging: LoggingSettings,
    pub gnudb: GnudbSettings,
    pub musicbrainz: MusicBrainzSettings,
    pub cue: CueSettings,
    pub lidarr: LidarrSettings,
    pub shnsplit: ShnsplitSettings,
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("could not determine an application data directory")]
    MissingProjectDirs,
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("gnudb.user_email must be a valid email when gnudb.disc_lookup_enabled is true")]
    MissingGnudbUserEmail,
    #[error("gnudb.server must be a hostname or unique code, not a URL or path: {0}")]
    InvalidGnudbServer(String),
    #[error("musicbrainz.base_url must be an HTTP(S) base URL: {0}")]
    InvalidMusicBrainzBaseUrl(String),
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
            .set_default("server.bind_address", "127.0.0.1:9899")?
            .set_default("logging.download_log_enabled", true)?
            .set_default("gnudb.disc_lookup_enabled", false)?
            .set_default("gnudb.server", "gnudb.gnudb.org")?
            .set_default("gnudb.user_email", "")?
            .set_default("musicbrainz.disc_lookup_enabled", true)?
            .set_default("musicbrainz.base_url", "https://musicbrainz.org")?
            .set_default("musicbrainz.trust_disc_lookup", false)?
            .set_default("musicbrainz.add_missing_release_group_enabled", false)?
            .set_default("cue.strict", false)?
            .set_default("lidarr.queue_page_size", 100)?
            .set_default("lidarr.queue_max_pages", 100)?
            .set_default("lidarr.manual_import_enabled", true)?
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

        let mut settings = config.try_deserialize::<Settings>()?;
        settings.gnudb.server = normalize_gnudb_server(&settings.gnudb.server)?;
        settings.musicbrainz.base_url =
            normalize_musicbrainz_base_url(&settings.musicbrainz.base_url)?;
        if settings.gnudb.disc_lookup_enabled && !looks_like_email(&settings.gnudb.user_email) {
            return Err(SettingsError::MissingGnudbUserEmail);
        }

        Ok(settings)
    }
}

fn normalize_musicbrainz_base_url(value: &str) -> Result<String, SettingsError> {
    let value = value.trim().trim_end_matches('/');
    if !value.starts_with("http://") && !value.starts_with("https://") {
        return Err(SettingsError::InvalidMusicBrainzBaseUrl(value.to_owned()));
    }

    let Some(origin) = value.split("://").nth(1) else {
        return Err(SettingsError::InvalidMusicBrainzBaseUrl(value.to_owned()));
    };
    if origin.is_empty() || origin.contains('/') || origin.contains('?') || origin.contains('#') {
        return Err(SettingsError::InvalidMusicBrainzBaseUrl(value.to_owned()));
    }

    Ok(value.to_owned())
}

fn normalize_gnudb_server(value: &str) -> Result<String, SettingsError> {
    let value = value.trim().trim_end_matches('.');
    if value.is_empty()
        || value.contains("://")
        || value.contains('/')
        || value.contains('?')
        || value.contains('#')
    {
        return Err(SettingsError::InvalidGnudbServer(value.to_owned()));
    }

    if value.eq_ignore_ascii_case("gnudb.gnudb.org")
        || value.to_ascii_lowercase().ends_with(".gnudb.org")
    {
        Ok(value.to_ascii_lowercase())
    } else {
        Ok(format!("{}.gnudb.org", value.to_ascii_lowercase()))
    }
}

fn looks_like_email(value: &str) -> bool {
    let value = value.trim();
    let Some((name, domain)) = value.split_once('@') else {
        return false;
    };
    !name.is_empty() && domain.contains('.') && !domain.ends_with('.')
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use tempfile::tempdir;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn download_log_is_enabled_by_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_test_env();
        let tmp = tempdir().unwrap();
        let config_path = tmp.path().join("splittarr.toml");
        fs::write(
            &config_path,
            r#"
[lidarr]
url = "http://lidarr"
api_key = "secret"
"#,
        )
        .unwrap();

        let settings =
            Settings::load_with_paths(Some(config_path), tmp.path().join("default"), None).unwrap();

        assert!(settings.logging.download_log_enabled);
    }

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
queue_page_size = 25
queue_max_pages = 20
manual_import_enabled = true

[server]
bind_address = "127.0.0.1:9899"

[logging]
download_log_enabled = false

[gnudb]
disc_lookup_enabled = true
server = "4ckgj7jx"
user_email = "user@example.com"

[musicbrainz]
disc_lookup_enabled = true
base_url = "https://musicbrainz.example/"
trust_disc_lookup = true
add_missing_release_group_enabled = true

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
        assert_eq!(settings.server.bind_address, "127.0.0.1:9899");
        assert!(!settings.logging.download_log_enabled);
        assert!(settings.gnudb.disc_lookup_enabled);
        assert_eq!(settings.gnudb.server, "4ckgj7jx.gnudb.org");
        assert_eq!(settings.gnudb.user_email, "user@example.com");
        assert!(settings.musicbrainz.disc_lookup_enabled);
        assert_eq!(settings.musicbrainz.base_url, "https://musicbrainz.example");
        assert!(settings.musicbrainz.trust_disc_lookup);
        assert!(settings.musicbrainz.add_missing_release_group_enabled);
        assert!(settings.cue.strict);
        assert_eq!(settings.lidarr.url, "http://lidarr");
        assert_eq!(settings.lidarr.queue_page_size, 25);
        assert_eq!(settings.lidarr.queue_max_pages, 20);
        assert!(settings.lidarr.manual_import_enabled);
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
        std::env::set_var("SPLITTARR_LIDARR__MANUAL_IMPORT_ENABLED", "true");
        std::env::set_var("SPLITTARR_LOGGING__DOWNLOAD_LOG_ENABLED", "false");
        std::env::set_var("SPLITTARR_GNUDB__DISC_LOOKUP_ENABLED", "true");
        std::env::set_var("SPLITTARR_GNUDB__SERVER", "abcd1234.gnudb.org");
        std::env::set_var("SPLITTARR_GNUDB__USER_EMAIL", "env@example.com");
        std::env::set_var("SPLITTARR_MUSICBRAINZ__DISC_LOOKUP_ENABLED", "true");
        std::env::set_var("SPLITTARR_MUSICBRAINZ__BASE_URL", "http://mb-env/");
        std::env::set_var("SPLITTARR_MUSICBRAINZ__TRUST_DISC_LOOKUP", "true");
        std::env::set_var(
            "SPLITTARR_MUSICBRAINZ__ADD_MISSING_RELEASE_GROUP_ENABLED",
            "true",
        );
        std::env::set_var("SPLITTARR_SERVER__BIND_ADDRESS", "0.0.0.0:1234");

        let settings =
            Settings::load_with_paths(Some(config_path), tmp.path().join("default"), None).unwrap();

        std::env::remove_var("SPLITTARR_CHECK_FREQUENCY_SECONDS");
        std::env::remove_var("SPLITTARR_LIDARR__URL");
        std::env::remove_var("SPLITTARR_LIDARR__MANUAL_IMPORT_ENABLED");
        std::env::remove_var("SPLITTARR_LOGGING__DOWNLOAD_LOG_ENABLED");
        std::env::remove_var("SPLITTARR_GNUDB__DISC_LOOKUP_ENABLED");
        std::env::remove_var("SPLITTARR_GNUDB__SERVER");
        std::env::remove_var("SPLITTARR_GNUDB__USER_EMAIL");
        std::env::remove_var("SPLITTARR_MUSICBRAINZ__DISC_LOOKUP_ENABLED");
        std::env::remove_var("SPLITTARR_MUSICBRAINZ__BASE_URL");
        std::env::remove_var("SPLITTARR_MUSICBRAINZ__TRUST_DISC_LOOKUP");
        std::env::remove_var("SPLITTARR_MUSICBRAINZ__ADD_MISSING_RELEASE_GROUP_ENABLED");
        std::env::remove_var("SPLITTARR_SERVER__BIND_ADDRESS");

        assert_eq!(settings.check_frequency_seconds, 9);
        assert_eq!(settings.lidarr.url, "http://from-env");
        assert_eq!(settings.lidarr.api_key, "file-secret");
        assert!(settings.lidarr.manual_import_enabled);
        assert!(!settings.logging.download_log_enabled);
        assert!(settings.gnudb.disc_lookup_enabled);
        assert_eq!(settings.gnudb.server, "abcd1234.gnudb.org");
        assert_eq!(settings.gnudb.user_email, "env@example.com");
        assert!(settings.musicbrainz.disc_lookup_enabled);
        assert_eq!(settings.musicbrainz.base_url, "http://mb-env");
        assert!(settings.musicbrainz.trust_disc_lookup);
        assert!(settings.musicbrainz.add_missing_release_group_enabled);
        assert_eq!(settings.server.bind_address, "0.0.0.0:1234");
    }

    #[test]
    fn gnudb_lookup_is_disabled_by_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_test_env();
        let tmp = tempdir().unwrap();
        let config_path = tmp.path().join("splittarr.toml");
        fs::write(
            &config_path,
            r#"
[lidarr]
url = "http://lidarr"
api_key = "secret"
"#,
        )
        .unwrap();

        let settings =
            Settings::load_with_paths(Some(config_path), tmp.path().join("default"), None).unwrap();

        assert!(!settings.gnudb.disc_lookup_enabled);
        assert_eq!(settings.gnudb.server, "gnudb.gnudb.org");
        assert_eq!(settings.gnudb.user_email, "");
        assert!(settings.musicbrainz.disc_lookup_enabled);
        assert_eq!(settings.musicbrainz.base_url, "https://musicbrainz.org");
        assert!(!settings.musicbrainz.trust_disc_lookup);
        assert!(!settings.musicbrainz.add_missing_release_group_enabled);
        assert!(settings.lidarr.manual_import_enabled);
    }

    #[test]
    fn gnudb_server_normalizes_bare_code_and_hostname() {
        assert_eq!(
            normalize_gnudb_server("4ckgj7jx").unwrap(),
            "4ckgj7jx.gnudb.org"
        );
        assert_eq!(
            normalize_gnudb_server("4ckgj7jx.gnudb.org").unwrap(),
            "4ckgj7jx.gnudb.org"
        );
        assert_eq!(
            normalize_gnudb_server("GNUDB.GNUDB.ORG").unwrap(),
            "gnudb.gnudb.org"
        );
    }

    #[test]
    fn gnudb_server_rejects_url_or_path_values() {
        assert!(matches!(
            normalize_gnudb_server("https://gnudb.gnudb.org/~cddb/cddb.cgi"),
            Err(SettingsError::InvalidGnudbServer(_))
        ));
        assert!(matches!(
            normalize_gnudb_server("gnudb.gnudb.org/~cddb/cddb.cgi"),
            Err(SettingsError::InvalidGnudbServer(_))
        ));
    }

    #[test]
    fn musicbrainz_base_url_normalizes_origin() {
        assert_eq!(
            normalize_musicbrainz_base_url("https://musicbrainz.org/").unwrap(),
            "https://musicbrainz.org"
        );
        assert_eq!(
            normalize_musicbrainz_base_url("http://musicbrainz.example").unwrap(),
            "http://musicbrainz.example"
        );
    }

    #[test]
    fn musicbrainz_base_url_rejects_path_query_or_fragment() {
        for value in [
            "https://musicbrainz.org/ws/2",
            "https://musicbrainz.org?x=y",
            "https://musicbrainz.org#fragment",
        ] {
            assert!(matches!(
                normalize_musicbrainz_base_url(value),
                Err(SettingsError::InvalidMusicBrainzBaseUrl(_))
            ));
        }
    }

    #[test]
    fn gnudb_lookup_requires_user_email_when_enabled() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_test_env();
        let tmp = tempdir().unwrap();
        let config_path = tmp.path().join("splittarr.toml");
        fs::write(
            &config_path,
            r#"
[lidarr]
url = "http://lidarr"
api_key = "secret"

[gnudb]
disc_lookup_enabled = true
"#,
        )
        .unwrap();

        let err = Settings::load_with_paths(Some(config_path), tmp.path().join("default"), None)
            .unwrap_err();

        assert!(matches!(err, SettingsError::MissingGnudbUserEmail));
    }

    fn clear_test_env() {
        std::env::remove_var("SPLITTARR_CHECK_FREQUENCY_SECONDS");
        std::env::remove_var("SPLITTARR_LIDARR__URL");
        std::env::remove_var("SPLITTARR_LIDARR__MANUAL_IMPORT_ENABLED");
        std::env::remove_var("SPLITTARR_LOGGING__DOWNLOAD_LOG_ENABLED");
        std::env::remove_var("SPLITTARR_GNUDB__DISC_LOOKUP_ENABLED");
        std::env::remove_var("SPLITTARR_GNUDB__SERVER");
        std::env::remove_var("SPLITTARR_GNUDB__USER_EMAIL");
        std::env::remove_var("SPLITTARR_MUSICBRAINZ__DISC_LOOKUP_ENABLED");
        std::env::remove_var("SPLITTARR_MUSICBRAINZ__BASE_URL");
        std::env::remove_var("SPLITTARR_MUSICBRAINZ__TRUST_DISC_LOOKUP");
        std::env::remove_var("SPLITTARR_MUSICBRAINZ__ADD_MISSING_RELEASE_GROUP_ENABLED");
        std::env::remove_var("SPLITTARR_SERVER__BIND_ADDRESS");
    }
}
