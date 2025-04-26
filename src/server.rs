use std::sync::Arc;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, Response, StatusCode};
use axum::response::IntoResponse;
use axum::Router;
use axum::routing::any;
use axum_proxy::AppendSuffix;
use tower_service::Service;
use crate::settings::Settings;

pub struct ProxyServer {
    settings: Settings
}

impl ProxyServer {
    
    pub fn new(settings: Settings) -> Self {
        Self {
            settings
        }
        
    }
    pub async fn run(self) -> Result<(), std::io::Error>{
        let listener = tokio::net::TcpListener::bind(self.settings.proxy_server_addr.clone())
            .await?;
        
        let app = Router::new()
            .route("/*path", any(handler))
            .route("/", any(handler))
            .with_state(Arc::new(self.settings));
        
        axum::serve(listener, app).await
    }
}

async fn handler(
    State(settings): State<Arc<Settings>>,
    request: Request<Body>,
) -> impl IntoResponse {
    let host = axum_proxy::builder_http(settings.target_url.clone()).unwrap();
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