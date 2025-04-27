use config::{Config, ConfigError, File};
use serde::Deserialize;


#[derive(Deserialize, Debug, Clone)]
pub struct Settings {
    #[serde(rename = "rate_limiter")]
    pub rate_limiter_settings: Vec<RateLimiterSettings>,

    #[serde(rename = "api_gateway")]
    pub api_gateway_settings: ApiGatewaySettings,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ApiGatewaySettings {
    pub target_url: String,
    pub proxy_server_addr: String,
}


#[derive(Deserialize, Debug, Clone)]
pub struct RateLimiterSettings {
    pub limit_type: String,
    pub tokens_count: u32,
}

impl Settings {
    pub fn new() -> Result<Settings, ConfigError> {
        let settings = Config::builder()
            .add_source(File::with_name("./Settings.toml"))
            .build()?;

        settings.try_deserialize()
    }
}