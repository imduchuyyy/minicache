<div align="center">
Your production-ready caching server with under 100 lines of code.
</div>

LRU Cache | HTTP Server | Configurable Capacity

## How to use
1. Run the server:
```bash
PORT=8080 CAPACITY=1000 minicache
```

2. Put an item in the cache:
```bash
curl -X PUT http://localhost:8080/mykey -d "myvalue"
```

3. Get an item from the cache:
```bash
curl http://localhost:8080/mykey
```

## Benchmarks
Avergage response times under load (100 concurrent clients):
- GET requests: 0.29ms
- PUT requests: 0.3ms