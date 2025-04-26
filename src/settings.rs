use config::{Config, ConfigError, File};
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
pub struct Settings {
    pub target_url: String,
    pub limit_type: String,
    pub tokens_count: u32,
    pub proxy_server_addr: String,
}

impl Settings {
    pub fn new() -> Result<Settings, ConfigError> {
        let settings = Config::builder()
            .add_source(File::with_name("./Settings.toml"))
            .build()?;

        settings.try_deserialize()
    }
}