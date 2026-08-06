use shmcache::ShmCache;

const APP: &str = "shmcache-demo";

fn main() -> Result<(), shmcache::Error> {
    let cache = ShmCache::open(APP, 256)?;
    println!("opened {APP:?} with {} slots", cache.capacity());

    cache.write(b"hello", b"world")?;

    match cache.read(b"hello") {
        Some(value) => println!("hello -> {}", String::from_utf8_lossy(&value)),
        None => println!("hello -> miss"),
    }

    ShmCache::unlink(APP)?;
    Ok(())
}
