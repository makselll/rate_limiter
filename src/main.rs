use std::sync::Arc;
use axum::{
    body::Body,
    extract::{State},
    http::{Request as HttpRequest, Response, StatusCode},
    response::IntoResponse,
    routing::any,
    Router,
};
use axum_proxy::{AppendSuffix};
use config::{Config, ConfigError, File};
use serde::Deserialize;
use tower_service::Service;

#[derive(Deserialize, Debug)]
struct Settings {
    pods_url: String,
    limit_type: String,
    tokens_count: u8,
}

impl Settings {
    fn new() -> Result<Settings, ConfigError> {
        let settings = Config::builder()
            .add_source(File::with_name("./Settings.toml"))
            .build()?;

        settings.try_deserialize()
    }
}

async fn handler(
    State(settings): State<Arc<Settings>>,
    request: HttpRequest<Body>,
) -> impl IntoResponse {
    let host = axum_proxy::builder_http(settings.pods_url.clone()).unwrap();
    let mut svc = host.build(AppendSuffix(""));
    
    match svc.call(request).await {
        Ok(response) => response.unwrap().into_response(),
        Err(err) => {
            eprintln!("Error: {}", err);
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("Internal Server Error"))
                .unwrap()
        }
    }
}

#[tokio::main]
async fn main() {
    let settings = Arc::new(Settings::new().expect("Failed to load settings"));
    
    let app = Router::new()
        .route("/*path", any(handler))
        .route("/", any(handler))
        .with_state(settings);
    
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .unwrap();

    axum::serve(listener, app).await.unwrap();
}