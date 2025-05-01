use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
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
    
    let rate_limiter_groups = vec!(
        &rate_limiter_manager.user_rate_limiters, // start to check the user
        &rate_limiter_manager.url_rate_limiters // check the urls
    );
    
    for rate_limiters_group in rate_limiter_groups {
        for rate_limiter in rate_limiters_group.iter() {
            let limit = rate_limiter.check(&safe_request, addr).await;
            match limit {
                None => continue,
                Some(limit) => {
                    match &lowest_limit {
                        Some(current) if current > &limit => lowest_limit = Some(limit),
                        None => lowest_limit = Some(limit),
                        _ => {}
                    }
                }
            }
        }

        if let Some(limit) = &lowest_limit {
            if limit.is_limit_exceeded {
                println!("Rate limit exceeded for {}", addr.ip());
                return (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded").into_response();
            }
        }
    }
    
    let mut response = next.run(Request::from_parts(safe_request.parts, Body::from(safe_request.body))).await;

    dbg!(&lowest_limit);
    
    if let Some(limit) = &lowest_limit {
        let headers = response.headers_mut();   
        headers.insert("X-RateLimit-Limit", HeaderValue::from(limit.total_limit));
        headers.insert("X-RateLimit-Remaining", HeaderValue::from(limit.requests_to_exceed_limit));
    }
    
    response
}

#[derive(Clone, Debug)]
pub enum Strategy {
    IP(IPRateLimiterStrategy),
    Url(UrlRateLimiterStrategy),
    Header(HeaderRateLimiterStrategy),
}

impl Strategy {
    pub fn from_possible_strategy(strategy: &PossibleStrategies) -> Self {
        match strategy {
            PossibleStrategies::IP => Strategy::IP(IPRateLimiterStrategy),
            PossibleStrategies::URL => Strategy::Url(UrlRateLimiterStrategy),
            PossibleStrategies::Header => Strategy::Header(HeaderRateLimiterStrategy),
        }
    }

    async fn check_limit(
        &self,
        redis_connection: Connection,
        global_bucket: Option<&Bucket>,
        buckets_per_value: Option<&HashMap<String, Bucket>>,
        request: &SafeRequest,
        addr: SocketAddr,
    ) -> Option<LimitForRequest> {
        match self {
            Strategy::IP(strategy) => strategy.check_limit(redis_connection, global_bucket, buckets_per_value, request, addr).await,
            Strategy::Url(strategy) => strategy.check_limit(redis_connection, global_bucket, buckets_per_value,request, addr).await,
            Strategy::Header(strategy) => strategy.check_limit(redis_connection, global_bucket, buckets_per_value,request, addr).await,
            
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
    pub fn new(rate_limiter_settings: RateLimiterSettings) -> Result<Self, std::io::Error> {
        let mut user_rate_limiters = Vec::new();
        let mut url_rate_limiters = Vec::new();


        let cfg = Config::from_url(format!("redis://{}", rate_limiter_settings.redis_addr.as_str()));
        let pool = cfg.create_pool(Some(deadpool_redis::Runtime::Tokio1)).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        for settings in rate_limiter_settings.limiters_settings.iter() {
            let strategy = Strategy::from_possible_strategy(&settings.strategy);
            let global_bucket= settings.global_bucket.as_ref().map(Bucket::from);
            
            let buckets_per_value = settings.buckets_per_value.as_ref().map(
                |buckets| buckets.iter().map(
                    |b| (b.value.clone(), Bucket::new(b.tokens_count, b.add_tokens_every))
                ).collect());
            
            if buckets_per_value.is_none() && global_bucket.is_none() {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,"No bucket defined for rate limiter"))
            }
            
            match strategy {
                Strategy::IP(_) => user_rate_limiters.push(Arc::new(RateLimiter::new(strategy, pool.clone(), global_bucket, buckets_per_value))),
                Strategy::Header(_) => user_rate_limiters.push(Arc::new(RateLimiter::new(strategy, pool.clone(), global_bucket, buckets_per_value))),
                Strategy::Url(_) => url_rate_limiters.push(Arc::new(RateLimiter::new(strategy, pool.clone(), global_bucket, buckets_per_value))),
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

impl Bucket {
    pub fn new(tokens_count: u32, add_tokens_every: u32) -> Self {
        Self {
            tokens_count,
            add_tokens_every,
        }
    }
}

impl From<&BucketSettings> for Bucket {
    fn from(settings: &BucketSettings) -> Self {
        Self {
            tokens_count: settings.tokens_count,
            add_tokens_every: settings.add_tokens_every,
        }
    }   
}
#[derive(Clone, Debug)]
struct RateLimiter {
    strategy: Strategy,
    redis_pool: Pool,
    global_bucket: Option<Bucket>,
    buckets_per_value: Option<HashMap<String, Bucket>>,
}


impl RateLimiter {
    pub fn new(strategy: Strategy, redis_pool: Pool, global_bucket: Option<Bucket>, buckets_per_value: Option<HashMap<String, Bucket>>) -> Self {
        Self {
            strategy,
            redis_pool,
            global_bucket,
            buckets_per_value,
        }
    }
    
    pub async fn check(&self, request: &SafeRequest, addr: SocketAddr) -> Option<LimitForRequest> {
        let redis_conn = match self.redis_pool.get().await {
            Ok(redis_conn) => redis_conn,
            Err(_) => return None,
        };
        self.strategy.check_limit(redis_conn, self.global_bucket.as_ref(), self.buckets_per_value.as_ref(), request, addr).await

    }
}


#[async_trait]
pub trait RateLimiterChecker {
    
    
    async fn check_limit(&self, mut redis_connection: Connection, global_bucket: Option<&Bucket>, buckets_per_value: Option<&HashMap<String, Bucket>>, request: &SafeRequest, addr: SocketAddr) -> Option<LimitForRequest> {
        let limit_redis_key = match self.get_redis_key(request, addr, global_bucket, buckets_per_value) {
            Some(key) => key,
            None => return None, // skip this check because we can't define what value we should check
        };
        dbg!(&limit_redis_key);
        
        redis::cmd("SET")
            .arg(&limit_redis_key.key)
            .arg(limit_redis_key.bucket.tokens_count)
            .arg("EX")
            .arg(limit_redis_key.bucket.add_tokens_every)
            .arg("NX")
            .query_async::<()>(&mut redis_connection)
            .await
            .unwrap_or(()); // Ignore error

        // Decrement key
        let count: i32 = redis::cmd("DECR")
            .arg(&limit_redis_key.key)
            .query_async(&mut redis_connection)
            .await
            .unwrap_or(-1); // Set to 0 if the key doesn't exist
        dbg!(&count);
        Some(LimitForRequest::new(limit_redis_key.bucket.tokens_count, count,count < 0))
    }
    
    fn hash_key(&self, s: String) -> u64 {
        let mut hasher = DefaultHasher::new();
        s.hash(&mut hasher);
        hasher.finish()
    }
    
    fn get_redis_key(&self, request: &SafeRequest, addr: SocketAddr, global_bucket: Option<&Bucket>, buckets_per_value: Option<&HashMap<String, Bucket>>) -> Option<LimitRedisKey>;
}


#[derive(Clone, Debug)]
pub struct IPRateLimiterStrategy;
#[derive(Clone, Debug)]
pub struct UrlRateLimiterStrategy;

#[derive(Clone, Debug)]
pub struct HeaderRateLimiterStrategy;


#[async_trait]
impl RateLimiterChecker for IPRateLimiterStrategy {
    fn get_redis_key(&self, _request: &SafeRequest, addr: SocketAddr, global_bucket: Option<&Bucket>, buckets_per_value: Option<&HashMap<String, Bucket>>) -> Option<LimitRedisKey> {
        let ip = addr.ip();

        let bucket = match buckets_per_value {
            Some(bucket) => bucket.get(&ip.to_string()).or(global_bucket),
            None => global_bucket
        };
        
        Some(LimitRedisKey::new(format!("rate_limiter:ip:{}", self.hash_key(ip.to_string())), bucket?.to_owned()))
    }
}


#[async_trait]
impl RateLimiterChecker for UrlRateLimiterStrategy {
    fn get_redis_key(&self, request: &SafeRequest, _addr: SocketAddr, global_bucket: Option<&Bucket>, buckets_per_value: Option<&HashMap<String, Bucket>>) -> Option<LimitRedisKey> {
        let uri = request.parts.uri.path();

        let bucket = match buckets_per_value {
            Some(bucket) => bucket.get(&uri.to_string()).or(global_bucket),
            None => global_bucket
        };
        
        Some(LimitRedisKey::new(format!("rate_limiter:url:{}", self.hash_key(uri.to_string())), bucket?.to_owned()))
    }
}


impl RateLimiterChecker for HeaderRateLimiterStrategy {
    fn get_redis_key(&self, request: &SafeRequest, _addr: SocketAddr, global_bucket: Option<&Bucket>, buckets_per_value: Option<&HashMap<String, Bucket>>) -> Option<LimitRedisKey> {
        let mut found_header: Option<String> = None;
        let mut found_bucket: Option<Bucket> = None;
        
        if let Some(bucket) = buckets_per_value {
            for (k, v) in bucket {
                match request.parts.headers.get(k.to_lowercase()) {
                    Some(value) => {
                        found_header = Some(value.to_str().unwrap().to_string());
                        found_bucket = Some(v.to_owned())
                    },
                    None => continue,
                };
            };
        };
        
        if found_header.is_none() && global_bucket.is_some() {
            if let Some(value) = request.parts.headers.get("authorization") {
                found_header = Some(value.to_str().unwrap().to_string());
                found_bucket = global_bucket.cloned();
            };
        }
        
        Some(LimitRedisKey::new(format!("rate_limiter:header:{}", self.hash_key(found_header.unwrap())), found_bucket?.to_owned()))
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
    total_limit: u32,
    requests_to_exceed_limit: i32,
    is_limit_exceeded: bool,
}

impl LimitForRequest {
    pub fn new(total_limit: u32, requests_to_exceed_limit: i32, is_limit_exceeded: bool) -> Self {
        Self {
            total_limit,
            requests_to_exceed_limit,
            is_limit_exceeded,
        }
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


#[derive(Debug)]
pub struct LimitRedisKey {
    key: String,
    bucket: Bucket,
}

impl LimitRedisKey {
    fn new(key: String, bucket: Bucket) -> Self {
        Self {
            key,
            bucket
        }
    }
}