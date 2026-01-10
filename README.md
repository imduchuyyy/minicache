
<div align="center">

# minicache
![Build Status](https://img.shields.io/github/actions/workflow/status/imduchuyyy/minicache/rust.yml?branch=main)
![License](https://img.shields.io/badge/license-GPL-blue.svg)

**A high-performance, memory-optimized LRU cache implementation in Rust.**

</div>

`minicache` is designed to be a lightweight, production-ready caching solution that balances speed and memory efficiency. It provides both a library for embedding in Rust applications and a standalone HTTP server.

## Features

- **🚀 High Performance**: Extremely fast operations (~16ns for updates) using optimized data structures.
- **💾 Memory Efficient**: Uses `u32` indices instead of pointers (`usize`) to reduce memory footprint by up to 50% on 64-bit systems.
- **⚡️ Zero Copy**: limit copying data where possible.
- **🛠 Production Ready**: Includes a fully functional HTTP server with configurable capacity.

## Benchmarks

I ran a benchmark on my local machine to test the raw throughput of the cache implementation (excluding network overhead). The results are impressive:

**Environment**: `cargo run --release`, MacBook Pro (Apple Silicon).
**Parameters**: 500k Capacity, 1M Operations.

| Operation | Throughput | Latency (Total) |
| :--- | :--- | :--- |
| **PUT** (Insert) | **~5.38 Million ops/sec** | 185ms (for 1M items) |
| **GET** (Hit) | **~12.09 Million ops/sec** | 8.27ms (for 100k items) |
| **GET** (Miss) | **~6.98 Million ops/sec** | 14.33ms (for 100k items) |

The use of `Bytes` and the index-based approach yields massive performance benefits, making this simple implementation competitive with production-grade systems.

## Usage

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

This project is licensed under the GPL License.
