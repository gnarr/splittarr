pub mod ports;

use anyhow::Result;

use crate::config::Settings;

pub async fn process_failed_lidarr_imports(settings: Settings) -> Result<()> {
    crate::app::run(settings).await
}
