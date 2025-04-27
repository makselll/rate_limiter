use std::collections::HashMap;
use std::net::SocketAddr;
use std::thread;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request as AxumRequest, State};
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use crate::settings::{RateLimiterSettings};

pub async fn middleware(
    State(rate_limiter_manager): State<Arc<RateLimiterManager>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let mut is_too_many_requests = false;
    for limiter in rate_limiter_manager.rate_limiters.iter() {
        if !limiter.check(&request, ConnectInfo(addr)) {
           is_too_many_requests = true;
        }
        
        if is_too_many_requests {
            return (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded").into_response();
        }
        
        dbg!(limiter);
    }

    next.run(request).await
}

#[derive(Clone, Debug)]
pub enum Strategy {
    IP(IPRateLimiterStrategy),
    Url(UrlRateLimiterStrategy),
}

impl Strategy {
    pub fn from_limit_type(limit_type: &str) -> Self {
        match limit_type {
            "ip" => Strategy::IP(IPRateLimiterStrategy),
            "url" => Strategy::Url(UrlRateLimiterStrategy),
            _ => panic!("Unknown limiter type"),
        }
    }
    
    fn check_limit(
        &self,
        data: &mut HashMap<String, u32>,
        default_limit: u32,
        request: &AxumRequest,
        addr: ConnectInfo<SocketAddr>,
    ) -> bool {
        match self {
            Strategy::IP(strategy) => strategy.check_limit(data, default_limit, request, addr),
            Strategy::Url(strategy) => strategy.check_limit(data, default_limit, request, addr),
        }
    }
    
}

pub struct RateLimiterManager {
    rate_limiters: Vec<RateLimiter>,
}

impl RateLimiterManager {
    pub fn new(rate_limiter_settings: Arc<Vec<RateLimiterSettings>>) -> Self {
        let mut rate_limiters = Vec::new();
        
        for setting in rate_limiter_settings.iter() {
            let strategy = Strategy::from_limit_type(&setting.limit_type);
            rate_limiters.push(RateLimiter::new(strategy, setting.tokens_count));
        }
        
        Self {
            rate_limiters
        }
    }
}




#[derive(Clone, Debug)]
struct RateLimiter {
    strategy: Strategy,
    default_limit: u32,
    data: Arc<RwLock<HashMap<String, u32>>>
}


impl RateLimiter {
    pub fn new(strategy: Strategy, tokens_count: u32) -> Self {
        let limiter = Self {
            strategy,
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


    pub fn check(&self, request: &AxumRequest, ConnectInfo(addr): ConnectInfo<SocketAddr>) -> bool {
        let mut data = self.data.write().unwrap();
        self.strategy.check_limit(&mut data, self.default_limit, request, ConnectInfo(addr))

    }
}


pub trait RateLimiterChecker {
    fn check_limit(&self, data: &mut HashMap<String, u32>, default_limit: u32, request: &AxumRequest, addr: ConnectInfo<SocketAddr>) -> bool;
}


#[derive(Clone, Debug)]
pub struct IPRateLimiterStrategy;
#[derive(Clone, Debug)]
pub struct UrlRateLimiterStrategy;


impl RateLimiterChecker for IPRateLimiterStrategy {
    fn check_limit(&self, data: &mut HashMap<String, u32>, default_limit: u32, _request: &AxumRequest, ConnectInfo(addr): ConnectInfo<SocketAddr>) -> bool {
        let ip = addr.ip().to_string();
        if let Some(count) = data.get_mut(&ip) {
            if *count == 0 {
                return false
            }
            *count -= 1
        } else {
            data.insert(ip, default_limit - 1);
        }
        true
    }   
}

impl RateLimiterChecker for UrlRateLimiterStrategy {
    fn check_limit(&self, data: &mut HashMap<String, u32>, default_limit: u32, request: &AxumRequest, ConnectInfo(_addr): ConnectInfo<SocketAddr>) -> bool {
        let uri = request.uri().to_string();
        if let Some(count) = data.get_mut(&uri) {
            if *count == 0 {
                return false
            }
            *count -= 1
        } else {
            data.insert(uri, default_limit - 1);
        }
        true
    }
}

