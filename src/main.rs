use axum::{
    extract::{Path, State},
    routing::get,
    Router,
    body::Bytes,
};
use minicache::ShardedCache;
use std::sync::Arc;
use std::env;

// ShardedCache handles locking internally
type SharedCache = Arc<ShardedCache>;

#[tokio::main]
async fn main() {
    let capacity: usize = env::var("CAPACITY")
        .unwrap_or_else(|_| "100".to_string())
        .parse()
        .expect("CAPACITY must be a number");

    let port = env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let addr = format!("127.0.0.1:{}", port);
    
    // Create sharded cache with 16 segments
    let shared_cache = Arc::new(ShardedCache::new(capacity, 16));

    // Both GET and PUT use the same path pattern
    let app = Router::new()
        .route("/{key}", 
            get(handle_get).put(handle_put)
        )
        .with_state(shared_cache);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("Simple Cache Server running on {}", addr);
    axum::serve(listener, app).await.unwrap();
}

/// GET /:key -> Retrieves value from LRU
async fn handle_get(
    Path(key): Path<String>,
    State(cache): State<SharedCache>,
) -> Bytes {
    let key = Bytes::from(key);

    match cache.get(&key) {
        Some(value) => value,
        None => Bytes::new(), // Return empty body if not found
    }
}

/// PUT /:key -> Sets value in LRU using the raw body
async fn handle_put(
    Path(key): Path<String>,
    State(cache): State<SharedCache>,
    body: Bytes,
) -> &'static str {
    let key = Bytes::from(key); // unavoidable (URL path)
    cache.put(key, body);
    "OK"
}