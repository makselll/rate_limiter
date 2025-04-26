use std::collections::HashMap;
use std::net::SocketAddr;
use std::thread;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request as AxumRequest, State};
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};


pub async fn middleware(
    State(limiter): State<Arc<RateLimiter>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !limiter.check(&request, ConnectInfo(addr)) {
        return (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded").into_response();
    }
    dbg!(limiter);

    next.run(request).await
}

#[derive(Clone, Debug)]
enum RateLimiterType {
    IP,
    User,
    Url
}

impl FromStr for RateLimiterType {
    type Err = ();
    fn from_str(input: &str) -> Result<RateLimiterType, Self::Err> {
        match input {
            "ip"   => Ok(RateLimiterType::IP),
            "user" => Ok(RateLimiterType::User),
            "url"  => Ok(RateLimiterType::Url),
            _      => Err(()),
        }
    }
}


#[derive(Clone, Debug)]
pub struct RateLimiter {
    limiter_type: RateLimiterType,
    default_limit: u32,
    data: Arc<RwLock<HashMap<String, u32>>>
}


impl RateLimiter {
    pub fn new(limiter_type: &str, tokens_count: u32) -> Self {
        let limiter = Self {
            limiter_type: limiter_type.parse().expect("Invalid limiter type"),
            default_limit: tokens_count,
            data: Arc::new(RwLock::new(HashMap::new()))
        };

        let data = Arc::clone(&limiter.data);
        thread::spawn({
            move || {
                loop {
                    thread::sleep(Duration::from_secs(60));
                    for (_, count) in data.write().unwrap().iter_mut() {
                        *count = tokens_count
                    }
                }
            }
        });

        limiter
    }

    fn check_ip_limit(&self, ip: String) -> bool {
        let mut data = self.data.write().unwrap();
        if let Some(count) = data.get_mut(&ip) {
            if *count == 0 {
                return false
            }
            *count -= 1
        } else {
            data.insert(ip, self.default_limit - 1);
        }
        true
    }

    fn check_url_limit(&self, url: String) -> bool {
        let mut data = self.data.write().unwrap();
        if let Some(count) = data.get_mut(&url) {
            if *count == 0 {
                return false
            }
            *count -= 1
        } else {
            data.insert(url, self.default_limit - 1);
        }
        true
    }

    pub fn check(&self, request: &AxumRequest, ConnectInfo(addr): ConnectInfo<SocketAddr>) -> bool {
        match self.limiter_type {
            RateLimiterType::IP => self.check_ip_limit(addr.ip().to_string()),
            RateLimiterType::User => true,
            RateLimiterType::Url => self.check_url_limit(request.uri().to_string()),
        }
    }
}