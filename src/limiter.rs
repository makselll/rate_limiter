use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::thread;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_macros::debug_middleware;
use deadpool_redis::{Config, Connection, Pool};

use crate::settings::{PossibleStrategies, RateLimiterSettings};


#[debug_middleware]
pub async fn middleware(
    State(rate_limiter_manager): State<Arc<RateLimiterManager>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    if rate_limiter_manager.ip_whitelist.contains(&addr.ip()) {
        println!("IP {} is whitelisted", addr.ip());
        return next.run(request).await;
    }

    let q_request: Request<Bytes> = Request::builder()
        .uri(request.uri())
        .method(request.method())
        .body(Bytes::from("hello")) // TODO replace with request body
        .unwrap();


    if !rate_limiter_manager.rate_limiters[0].check(q_request).await {
        println!("Rate limit exceeded for {}", addr.ip());
        return (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded").into_response();
    }

    next.run(request).await
}

#[derive(Clone, Debug)]
pub enum Strategy {
    IP(IPRateLimiterStrategy),
    Url(UrlRateLimiterStrategy),
}

impl Strategy {
    pub fn from_possible_strategy(strategy: &PossibleStrategies) -> Self {
        match strategy {
            PossibleStrategies::IP => Strategy::IP(IPRateLimiterStrategy),
            PossibleStrategies::URL => Strategy::Url(UrlRateLimiterStrategy),
        }
    }

    fn check_limit(
        &self,
        redis_connection: Connection,
        default_limit: u32,
        request: Request<Bytes>,
        addr: SocketAddr,
    ) -> bool {
        match self {
            Strategy::IP(strategy) => strategy.check_limit(redis_connection, default_limit, request, addr),
            Strategy::Url(strategy) => strategy.check_limit(redis_connection, default_limit, request, addr),
        }
    }

}

#[derive(Clone, Debug)]
pub struct RateLimiterManager {
    redis_addr: String,
    ip_whitelist: HashSet<IpAddr>,
    rate_limiters: Vec<Arc<RateLimiter>>,
}

impl RateLimiterManager {
    pub fn new(rate_limiter_settings: Arc<RateLimiterSettings>) -> Result<Self, deadpool_redis::CreatePoolError> {
        let mut rate_limiters = Vec::new();

        let cfg = Config::from_url(format!("redis://{}", rate_limiter_settings.redis_addr.as_str()));
        let pool = cfg.create_pool(Some(deadpool_redis::Runtime::Tokio1))?;

        for setting in rate_limiter_settings.limiters_settings.iter() {
            let strategy = Strategy::from_possible_strategy(&setting.strategy);
            rate_limiters.push(Arc::new(RateLimiter::new(strategy, setting.tokens_count, pool.clone())));
        }

        Ok(Self {
            rate_limiters,
            redis_addr: rate_limiter_settings.redis_addr.clone(),
            ip_whitelist: rate_limiter_settings.ip_whitelist.clone(),
        })
    }
}




#[derive(Clone, Debug)]
struct RateLimiter {
    strategy: Strategy,
    default_limit: u32,
    data: Arc<RwLock<HashMap<String, u32>>>,
    redis_pool: Pool
}


impl RateLimiter {
    pub fn new(strategy: Strategy, tokens_count: u32, redis_pool: Pool) -> Self {
        let limiter = Self {
            strategy,
            default_limit: tokens_count,
            data: Arc::new(RwLock::new(HashMap::new())),
            redis_pool,
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
    
    pub async fn check(&self, request: Request<Bytes>) -> bool {

        let redis_conn = match self.redis_pool.get().await {
            Ok(redis_conn) => redis_conn,
            Err(_) => return false
        };
        self.strategy.check_limit(redis_conn, self.default_limit, request, SocketAddr::new(IpAddr::V4("127.0.0.1".parse().unwrap()), 8080))

    }
}


pub trait RateLimiterChecker {
    fn check_limit(&self, redis_connection: Connection, default_limit: u32, request: Request<Bytes>, addr: SocketAddr) -> bool;
}


#[derive(Clone, Debug)]
pub struct IPRateLimiterStrategy;
#[derive(Clone, Debug)]
pub struct UrlRateLimiterStrategy;


impl RateLimiterChecker for IPRateLimiterStrategy {
    fn check_limit(&self, redis_connection: Connection, default_limit: u32, _request: Request<Bytes>, addr: SocketAddr) -> bool {
        // let ip = addr.ip().to_string();
        // if let Some(count) = data.get_mut(&ip) {
        //     if *count == 0 {
        //         return false
        //     }
        //     *count -= 1
        // } else {
        //     data.insert(ip, default_limit - 1);
        // }
        true
    }
}

impl RateLimiterChecker for UrlRateLimiterStrategy {
    fn check_limit(&self, redis_connection: Connection, default_limit: u32, request: Request<Bytes>, _addr: SocketAddr) -> bool {
        // let uri = request.uri().to_string();
        // if let Some(count) = data.get_mut(&uri) {
        //     if *count == 0 {
        //         return false
        //     }
        //     *count -= 1
        // } else {
        //     data.insert(uri, default_limit - 1);
        // }
        true
    }
}

