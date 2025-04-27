use std::net::SocketAddr;
use std::sync::Arc;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, Response, StatusCode};
use axum::middleware::{from_fn_with_state};
use axum::response::IntoResponse;
use axum::Router;
use axum::routing::any;
use axum_proxy::AppendSuffix;
use tower_service::Service;
use crate::limiter;
use crate::limiter::{RateLimiterManager};
use crate::settings::{ApiGatewaySettings, Settings};

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
        let listener = tokio::net::TcpListener::bind(self.settings.api_gateway_settings.proxy_server_addr.clone())
            .await?;

        let limiter = Arc::new(RateLimiterManager::new(Arc::new(self.settings.rate_limiter_settings)).unwrap());
        
        let app = Router::new()
            .route("/*path", any(handler))
            .route("/", any(handler))
            .layer(from_fn_with_state(limiter, limiter::middleware))
            .with_state(Arc::new(self.settings.api_gateway_settings));

        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await
    }
}

async fn handler(
    State(settings): State<Arc<ApiGatewaySettings>>,
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