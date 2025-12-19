mod cache;

use cache::Cache;
use axum::{
    extract::{Path, State},
    routing::get,
    Router,
    body::Bytes,
};
use std::sync::{Arc, Mutex};
use std::env;

// Assume ConcurrentCache is the struct we built previously
type SharedCache = Arc<Mutex<Cache>>;

#[tokio::main]
async fn main() {
    let capacity: usize = env::var("CAPACITY")
        .unwrap_or_else(|_| "100".to_string())
        .parse()
        .expect("CAPACITY must be a number");

    let port = env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let addr = format!("127.0.0.1:{}", port);
    let shared_cache = Arc::new(Mutex::new(Cache::new(capacity)));

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
) -> String {
    let mut cache = cache.lock().unwrap();
    match cache.get(&key.into_bytes()) {
        Some(value) => String::from_utf8_lossy(&value).to_string(),
        None => "Key not found".to_string(),
    }
}

/// PUT /:key -> Sets value in LRU using the raw body
async fn handle_put(
    Path(key): Path<String>,
    State(cache): State<SharedCache>,
    body: Bytes, // Extracts the raw body (-d)
) -> &'static str {
    let mut cache = cache.lock().unwrap();
    cache.push(key.into_bytes(), body.to_vec());
    "OK"
}