use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::thread;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{Request, StatusCode};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_macros::debug_middleware;
use deadpool_redis::{redis, Config, Connection, Pool};
use deadpool_redis::redis::AsyncCommands;
use crate::settings::{PossibleStrategies, RateLimiterSettings};


#[debug_middleware]
pub async fn middleware(
    State(rate_limiter_manager): State<Arc<RateLimiterManager>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    // Check whitelist
    if rate_limiter_manager.ip_whitelist.contains(&addr.ip()) {
        println!("IP {} is whitelisted", addr.ip());
        return next.run(request).await;
    }

    // Split the request into parts and body because Request<Body> is not Send
    let (parts, body) = request.into_parts();
    let body_bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response(),
    };
    
    let safe_request = SafeRequest::new(parts, body_bytes);
    let mut is_limit_exceeded = false;
    
    for rate_limiter in rate_limiter_manager.user_rate_limiters.iter() {
        if !rate_limiter.check(&safe_request).await {
            is_limit_exceeded = true;
        }
    }

    if is_limit_exceeded {
        println!("Rate limit exceeded for {}", addr.ip());
        return (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded").into_response();
    }

    for rate_limiter in rate_limiter_manager.url_rate_limiters.iter() {
        if !rate_limiter.check(&safe_request).await {
            is_limit_exceeded = true;
        }
    }

    if is_limit_exceeded {
        println!("Rate limit exceeded for {}", addr.ip());
        return (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded").into_response();
    }

    next.run(Request::from_parts(safe_request.parts, Body::from(safe_request.body))).await
}
struct SafeRequest {
    parts: Parts,
    body: Bytes,
}

impl SafeRequest {
    pub fn new(parts: Parts, body: Bytes) -> Self {
        Self {
            parts,
            body,
        }
    }
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

    async fn check_limit(
        &self,
        redis_connection: Connection,
        default_limit: u32,
        request: &SafeRequest,
        addr: SocketAddr,
    ) -> bool {
        match self {
            Strategy::IP(strategy) => strategy.check_limit(redis_connection, default_limit, request, addr).await,
            Strategy::Url(strategy) => strategy.check_limit(redis_connection, default_limit, request, addr).await,
        }
    }

}

#[derive(Clone, Debug)]
pub struct RateLimiterManager {
    redis_addr: String,
    ip_whitelist: HashSet<IpAddr>,
    user_rate_limiters: Vec<Arc<RateLimiter>>,
    url_rate_limiters: Vec<Arc<RateLimiter>>,
}

impl RateLimiterManager {
    pub fn new(rate_limiter_settings: Arc<RateLimiterSettings>) -> Result<Self, deadpool_redis::CreatePoolError> {
        let mut user_rate_limiters = Vec::new();
        let mut url_rate_limiters = Vec::new();


        let cfg = Config::from_url(format!("redis://{}", rate_limiter_settings.redis_addr.as_str()));
        let pool = cfg.create_pool(Some(deadpool_redis::Runtime::Tokio1))?;

        for setting in rate_limiter_settings.limiters_settings.iter() {
            let strategy = Strategy::from_possible_strategy(&setting.strategy);
            match strategy {
                Strategy::IP(_) => user_rate_limiters.push(Arc::new(RateLimiter::new(strategy, setting.tokens_count, pool.clone()))),
                Strategy::Url(_) => url_rate_limiters.push(Arc::new(RateLimiter::new(strategy, setting.tokens_count, pool.clone()))),   
            }
        }
        
        Ok(Self {
            user_rate_limiters,
            url_rate_limiters,
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
    
    pub async fn check(&self, request: &SafeRequest) -> bool {

        let redis_conn = match self.redis_pool.get().await {
            Ok(redis_conn) => redis_conn,
            Err(_) => return false
        };
        self.strategy.check_limit(redis_conn, self.default_limit, request, SocketAddr::new(IpAddr::V4("127.0.0.1".parse().unwrap()), 8080)).await

    }
}


pub trait RateLimiterChecker {
    async fn check_limit(&self, redis_connection: Connection, default_limit: u32, request: &SafeRequest, addr: SocketAddr) -> bool;
    
    fn get_redis_key(&self, request: &SafeRequest, addr: SocketAddr) -> String;
}


#[derive(Clone, Debug)]
pub struct IPRateLimiterStrategy;
#[derive(Clone, Debug)]
pub struct UrlRateLimiterStrategy;


impl RateLimiterChecker for IPRateLimiterStrategy {
    async fn check_limit(&self, mut redis_connection: Connection, default_limit: u32, _request: &SafeRequest, addr: SocketAddr) -> bool {
        let key = self.get_redis_key(_request, addr);
        redis::cmd("SET")
            .arg(&key)
            .arg(default_limit)
            .arg("EX")
            .arg(60)
            .arg("NX")
            .query_async::<()>(&mut redis_connection)
            .await
            .unwrap_or(()); // Ignore error

        // Decrement key
        let count: i32 = redis::cmd("DECR")
            .arg(&key)
            .query_async(&mut redis_connection)
            .await
            .unwrap_or(-1); // Set to 0 if the key doesn't exist

        dbg!(&count);
        
        
        if count < 0 {
            false
        } else {
            true
        }

    }
    
    fn get_redis_key(&self, _request: &SafeRequest, addr: SocketAddr) -> String {
        let ip = addr.ip();
        String::from(format!("rate_limiter:ip:{}", ip))
    }
}

impl RateLimiterChecker for UrlRateLimiterStrategy {
    async fn check_limit(&self, mut redis_connection: Connection, default_limit: u32, request: &SafeRequest, addr: SocketAddr) -> bool {
        let key = self.get_redis_key(request, addr);
        
        // Set key if not exists
        redis::cmd("SET")
            .arg(&key)
            .arg(default_limit)
            .arg("EX")
            .arg(60)
            .arg("NX")
            .query_async::<()>(&mut redis_connection)
            .await
            .unwrap_or(()); // Ignore error

        // Decrement key
        let count: i32 = redis::cmd("DECR")
            .arg(&key)
            .query_async(&mut redis_connection)
            .await
            .unwrap_or(-1); // Set to 0 if the key doesn't exist

        dbg!(&count);
        
        if count < 0 {
            false
        } else {
            true
        }

    }

    fn get_redis_key(&self, request: &SafeRequest, _addr: SocketAddr) -> String {
        let uri = &request.parts.uri;
        String::from(format!("rate_limiter:url:{}", uri))
    }
}

