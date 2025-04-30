use std::collections::{HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc};
use axum::async_trait;
use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderValue, Request, StatusCode};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_macros::debug_middleware;
use deadpool_redis::{redis, Config, Connection, Pool};
use crate::settings::{BucketSettings, PossibleStrategies, RateLimiterSettings};


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
    let mut lowest_limit: Option<LimitForRequest> = None;
    
    for rate_limiter in rate_limiter_manager.user_rate_limiters.iter() {
        let limit = rate_limiter.check(&safe_request, addr).await;
        match &lowest_limit {
            Some(current) if current > &limit => lowest_limit = Some(limit),
            None => lowest_limit = Some(limit),
            _ => {}
        }
    }
    
    if let Some(limit) = &lowest_limit {
        if limit.is_limit_exceeded {
            println!("Rate limit exceeded for {}", addr.ip());
            return (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded").into_response();
        }
    }
    
    for rate_limiter in rate_limiter_manager.url_rate_limiters.iter() {
        let limit = rate_limiter.check(&safe_request, addr).await;
        match &lowest_limit {
            Some(current) if current > &limit => lowest_limit = Some(limit),
            None => lowest_limit = Some(limit),
            _ => {}
        }
    }
    
    if let Some(limit) = &lowest_limit {
        if limit.is_limit_exceeded {
            println!("Rate limit exceeded for {}", addr.ip());
            return (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded").into_response();
        }
    }

    let mut response = next.run(Request::from_parts(safe_request.parts, Body::from(safe_request.body))).await;

    if let Some(limit) = &lowest_limit {
        let headers = response.headers_mut();   
        headers.insert("X-RateLimit-Limit", HeaderValue::from(limit.total_limit.unwrap()));
        headers.insert("X-RateLimit-Remaining", HeaderValue::from(limit.requests_to_exceed_limit.unwrap()));
    }
    
    response
}

#[derive(Clone, Debug)]
pub enum Strategy {
    IP(IPRateLimiterStrategy),
    Url(UrlRateLimiterStrategy),
    AuthHeader(AuthHeaderRateLimiterStrategy),
}

impl Strategy {
    pub fn from_possible_strategy(strategy: &PossibleStrategies) -> Self {
        match strategy {
            PossibleStrategies::IP => Strategy::IP(IPRateLimiterStrategy),
            PossibleStrategies::URL => Strategy::Url(UrlRateLimiterStrategy),
            PossibleStrategies::AuthHeader => Strategy::AuthHeader(AuthHeaderRateLimiterStrategy),
        }
    }

    async fn check_limit(
        &self,
        redis_connection: Connection,
        bucket: &Bucket,
        request: &SafeRequest,
        addr: SocketAddr,
    ) -> LimitForRequest {
        match self {
            Strategy::IP(strategy) => strategy.check_limit(redis_connection, bucket, request, addr).await,
            Strategy::Url(strategy) => strategy.check_limit(redis_connection, bucket, request, addr).await,
            Strategy::AuthHeader(strategy) => strategy.check_limit(redis_connection, bucket, request, addr).await,
            
        }
    }

}

#[derive(Clone, Debug)]
pub struct RateLimiterManager {
    ip_whitelist: HashSet<IpAddr>,
    user_rate_limiters: Vec<Arc<RateLimiter>>,
    url_rate_limiters: Vec<Arc<RateLimiter>>,
}

impl RateLimiterManager {
    pub fn new(rate_limiter_settings: RateLimiterSettings) -> Result<Self, deadpool_redis::CreatePoolError> {
        let mut user_rate_limiters = Vec::new();
        let mut url_rate_limiters = Vec::new();


        let cfg = Config::from_url(format!("redis://{}", rate_limiter_settings.redis_addr.as_str()));
        let pool = cfg.create_pool(Some(deadpool_redis::Runtime::Tokio1))?;

        for setting in rate_limiter_settings.limiters_settings.iter() {
            let strategy = Strategy::from_possible_strategy(&setting.strategy);
            match strategy {
                Strategy::IP(_) => user_rate_limiters.push(Arc::new(RateLimiter::new(strategy, pool.clone(), Bucket::from(&setting.bucket)))),
                Strategy::AuthHeader(_) => user_rate_limiters.push(Arc::new(RateLimiter::new(strategy, pool.clone(), Bucket::from(&setting.bucket)))),
                Strategy::Url(_) => url_rate_limiters.push(Arc::new(RateLimiter::new(strategy, pool.clone(), Bucket::from(&setting.bucket)))),   
            }
        }
        
        Ok(Self {
            user_rate_limiters,
            url_rate_limiters,
            ip_whitelist: rate_limiter_settings.ip_whitelist.clone(),
        })
    }
}


#[derive(Clone, Debug)]
pub struct Bucket {
    tokens_count: u32,
    add_tokens_every: u32,
}

impl From<&BucketSettings> for Bucket {
    fn from(bucket_settings: &BucketSettings) -> Self {
        Self {
            tokens_count: bucket_settings.tokens_count,
            add_tokens_every: bucket_settings.add_tokens_every,
        }
    }   
}


#[derive(Clone, Debug)]
struct RateLimiter {
    strategy: Strategy,
    redis_pool: Pool,
    bucket: Bucket,
}


impl RateLimiter {
    pub fn new(strategy: Strategy, redis_pool: Pool, bucket: Bucket) -> Self {
        Self {
            strategy,
            bucket,
            redis_pool,
        }
    }
    
    pub async fn check(&self, request: &SafeRequest, addr: SocketAddr) -> LimitForRequest {
        let redis_conn = match self.redis_pool.get().await {
            Ok(redis_conn) => redis_conn,
            Err(_) => return LimitForRequest::default(),
        };
        self.strategy.check_limit(redis_conn, &self.bucket, request, addr).await

    }
}


#[async_trait]
pub trait RateLimiterChecker {
    async fn check_limit(&self, mut redis_connection: Connection, bucket: &Bucket, request: &SafeRequest, addr: SocketAddr) -> LimitForRequest {
        let key = match self.get_redis_key(request, addr) {
            Some(key) => key,
            None => return LimitForRequest::default(), // skip this check because we can't define what value we should check
        };
        
        redis::cmd("SET")
            .arg(&key)
            .arg(bucket.tokens_count)
            .arg("EX")
            .arg(bucket.add_tokens_every)
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

        LimitForRequest::new(Some(bucket.tokens_count), Some(count),count < 0)
    }
    
    fn get_redis_key(&self, request: &SafeRequest, addr: SocketAddr) -> Option<String>;
}


#[derive(Clone, Debug)]
pub struct IPRateLimiterStrategy;
#[derive(Clone, Debug)]
pub struct UrlRateLimiterStrategy;

#[derive(Clone, Debug)]
pub struct AuthHeaderRateLimiterStrategy;


#[async_trait]
impl RateLimiterChecker for IPRateLimiterStrategy {
    fn get_redis_key(&self, _request: &SafeRequest, addr: SocketAddr) -> Option<String> {
        let ip = addr.ip();
        Some(format!("rate_limiter:ip:{}", ip))
    }
}


#[async_trait]
impl RateLimiterChecker for UrlRateLimiterStrategy {
    fn get_redis_key(&self, request: &SafeRequest, _addr: SocketAddr) -> Option<String> {
        let uri = &request.parts.uri;
        Some(format!("rate_limiter:url:{}", uri))
    }
}


impl RateLimiterChecker for AuthHeaderRateLimiterStrategy {
    fn get_redis_key(&self, request: &SafeRequest, _addr: SocketAddr) -> Option<String> {
        let auth_header = request.parts.headers.get("authorization")?.to_str().ok()?;
        Some(format!("rate_limiter:auth_header:{}", auth_header))
    }
}

pub struct SafeRequest {
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
pub struct LimitForRequest {
    total_limit: Option<u32>,
    requests_to_exceed_limit: Option<i32>,
    is_limit_exceeded: bool,
}

impl LimitForRequest {
    pub fn new(total_limit: Option<u32>, requests_to_exceed_limit: Option<i32>, is_limit_exceeded: bool) -> Self {
        Self {
            total_limit,
            requests_to_exceed_limit,
            is_limit_exceeded,
        }
    }
}

impl Default for LimitForRequest {
    fn default() -> Self {
        Self::new(None, None, false)
    }
}

impl PartialEq for LimitForRequest {
    fn eq(&self, other: &Self) -> bool {
        self.requests_to_exceed_limit == other.requests_to_exceed_limit
    }
}

impl Eq for LimitForRequest {}

impl Ord for LimitForRequest {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.requests_to_exceed_limit.cmp(&other.requests_to_exceed_limit)
    }
}

impl PartialOrd for LimitForRequest {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.requests_to_exceed_limit.cmp(&other.requests_to_exceed_limit))
    }
}