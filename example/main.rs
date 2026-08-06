use minicache::ShmCache;

const APP: &str = "minicache-demo";

fn main() -> Result<(), minicache::Error> {
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
