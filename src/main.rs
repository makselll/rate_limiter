use rate_limiter::server::ProxyServer;
use rate_limiter::settings::Settings;

#[tokio::main]
async fn main() {
    let settings = Settings::new().expect("Failed to load settings");
    
    let server = ProxyServer::new(settings);
    server.run().await.expect("Failed to run server");
}