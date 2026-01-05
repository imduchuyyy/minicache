# minicache

<div align="center">

![Build Status](https://img.shields.io/github/actions/workflow/status/imduchuyyy/minicache/rust.yml?branch=main)
![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Crates.io](https://img.shields.io/crates/v/minicache.svg)

**A high-performance, memory-optimized LRU cache implementation in Rust.**

</div>

`minicache` is designed to be a lightweight, production-ready caching solution that balances speed and memory efficiency. It provides both a library for embedding in Rust applications and a standalone HTTP server.

## Features

- **🚀 High Performance**: Extremely fast operations (~16ns for updates) using optimized data structures.
- **💾 Memory Efficient**: Uses `u32` indices instead of pointers (`usize`) to reduce memory footprint by up to 50% on 64-bit systems.
- **⚡️ Zero Copy**: limit copying data where possible.
- **🛠 Production Ready**: Includes a fully functional HTTP server with configurable capacity.

## Benchmarks

### Core Library Performance
Benchmarks run on Apple Silicon (M-series).

| Operation | Time (ns) | Description |
|-----------|-----------|-------------|
| `put` (overwrite) | **~16 ns** | Update existing key |
| `put` (evict) | **~117 ns** | Insert new item causing eviction |

### HTTP Server Performance
Average response times under load (100 concurrent clients):
- **GET requests**: 0.29ms
- **PUT requests**: 0.30ms

## Usage

### As a Library
Add this to your `Cargo.toml`:

```toml
[dependencies]
minicache = "0.1.0"
bytes = "1.0"
```

```rust
use minicache::Cache;
use bytes::Bytes;

fn main() {
    let mut cache = Cache::new(1000); // Capacity: 1000 items

    let key = Bytes::from("my_key");
    let value = Bytes::from("my_value");

    cache.put(key.clone(), value);
    
    if let Some(val) = cache.get(&key) {
        println!("Found: {:?}", val);
    }
}
```

### As a Standalone Server

1. **Start the server**:
   ```bash
   PORT=8080 CAPACITY=1000 cargo run --release
   ```

2. **Store a value**:
   ```bash
   curl -X PUT http://localhost:8080/mykey -d "myvalue"
   ```

3. **Retrieve a value**:
   ```bash
   curl http://localhost:8080/mykey
   ```

## License

This project is licensed under the MIT License.