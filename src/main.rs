mod cache;

use std::sync::{Arc, Mutex};
use crate::cache::Cache;

type SharedCache = Arc<Mutex<Cache>>;

#[tokio::main]
async fn main() {
    let cache: SharedCache = Arc::new(Mutex::new(Cache::new(100)));

    // Clone the Arc for the async task
    let cache_clone = Arc::clone(&cache);
    
    tokio::spawn(async move {
        let mut c = cache_clone.lock().unwrap();
        c.push(vec![1], vec![100]);

        // child thread access the cache by the key pushed by main thread
        if let Some(value) = c.get(&vec![2]) {
            println!("Value for key [2]: {:?}", value);
        } else {
            println!("Key [2] not found in cache.");
        }
    });

    // Main thread can also access the cache
    {
        let mut c = cache.lock().unwrap();
        c.push(vec![2], vec![200]);
    }

}