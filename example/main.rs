//! Open a shared-memory cache, write one key, read it back.
//!
//! Run a second copy of this while the first is alive and it attaches to the same
//! cache — the name is the only thing the two processes have to agree on.

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

    // Without this the object survives until reboot, since it outlives every process
    // that used it.
    ShmCache::unlink(APP)?;
    Ok(())
}
