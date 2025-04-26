use config::{Config, ConfigError, File};
use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct Settings {
    pub target_url: String,
    pub limit_type: String,
    pub tokens_count: u8,
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