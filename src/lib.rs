pub mod shm;

use bytes::Bytes;
use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::fmt::Display;
use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::Mutex;

type Key = Bytes;
type Value = Bytes;
type Index = u32;

#[derive(Debug)]
struct Entry {
    key: Key,
    value: Value,
    prev: Option<Index>,
    next: Option<Index>,
}

#[derive(Debug)]
pub struct LruCache {
    capacity: usize,
    map: HashMap<Key, Index>,
    entries: Vec<Entry>,
    head: Option<Index>,
    tail: Option<Index>,
    free: Vec<Index>, // reuse slot
}

impl LruCache {
    pub fn new(capacity: usize) -> Self {
        assert!(
            capacity <= u32::MAX as usize,
            "Capacity too large for u32 index"
        );
        LruCache {
            capacity,
            map: HashMap::with_capacity(capacity),
            entries: Vec::with_capacity(capacity),
            head: None,
            tail: None,
            free: Vec::new(),
        }
    }

    pub fn get(&mut self, key: &Key) -> Option<Value> {
        let idx = *self.map.get(key)?;
        self.move_to_head(idx);
        Some(self.entries[idx as usize].value.clone()) // O(1)
    }

    pub fn put(&mut self, key: Key, value: Value) {
        if let Some(&idx) = self.map.get(&key) {
            self.entries[idx as usize].value = value;
            self.move_to_head(idx);
            return;
        }

        if self.map.len() == self.capacity {
            self.evict();
        }

        let idx = self.alloc(Entry {
            key: key.clone(),
            value,
            prev: None,
            next: None,
        });

        self.map.insert(key, idx);
        self.attach_head(idx);
    }

    fn move_to_head(&mut self, idx: Index) {
        self.detach(idx);
        self.attach_head(idx);
    }

    fn attach_head(&mut self, idx: Index) {
        self.entries[idx as usize].prev = None;
        self.entries[idx as usize].next = self.head;

        if let Some(old_head) = self.head {
            self.entries[old_head as usize].prev = Some(idx);
        } else {
            self.tail = Some(idx);
        }

        self.head = Some(idx);
    }

    fn detach(&mut self, idx: Index) {
        let (prev, next) = {
            let entry = &self.entries[idx as usize];
            (entry.prev, entry.next)
        };

        if let Some(prev_idx) = prev {
            self.entries[prev_idx as usize].next = next;
        } else {
            self.head = next;
        }

        if let Some(next_idx) = next {
            self.entries[next_idx as usize].prev = prev;
        } else {
            self.tail = prev;
        }

        self.entries[idx as usize].prev = None;
        self.entries[idx as usize].next = None;
    }

    fn alloc(&mut self, entry: Entry) -> Index {
        if let Some(idx) = self.free.pop() {
            self.entries[idx as usize] = entry;
            idx
        } else {
            let idx = self.entries.len();
            self.entries.push(entry);
            idx as Index
        }
    }

    fn evict(&mut self) {
        if let Some(tail_idx) = self.tail {
            let key = self.entries[tail_idx as usize].key.clone();
            self.detach(tail_idx);
            self.map.remove(&key);
            self.free.push(tail_idx);
        }
    }
}

impl Display for LruCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut idx = self.head;
        write!(f, "LruCache [")?;
        while let Some(i) = idx {
            let entry = &self.entries[i as usize];
            write!(f, "({:?}: {:?}) ", entry.key, entry.value)?;
            idx = entry.next;
        }
        write!(f, "]")
    }
}

#[derive(Debug)]
pub struct ShardedCache {
    segments: Vec<Mutex<LruCache>>,
    hasher: RandomState,
    num_shards: usize,
}

impl ShardedCache {
    pub fn new(total_capacity: usize, num_shards: usize) -> Self {
        assert!(num_shards > 0, "num_shards must be > 0");
        let seg_capacity = (total_capacity + num_shards - 1) / num_shards;
        let mut segments = Vec::with_capacity(num_shards);
        for _ in 0..num_shards {
            segments.push(Mutex::new(LruCache::new(seg_capacity)));
        }
        ShardedCache {
            segments,
            hasher: RandomState::new(),
            num_shards,
        }
    }

    pub fn get(&self, key: &Key) -> Option<Value> {
        let idx = self.get_shard_index(key);
        let mut shard = self.segments[idx].lock().unwrap();
        shard.get(key)
    }

    pub fn put(&self, key: Key, value: Value) {
        let idx = self.get_shard_index(&key);
        let mut shard = self.segments[idx].lock().unwrap();
        shard.put(key, value)
    }

    fn get_shard_index(&self, key: &Key) -> usize {
        let mut s = self.hasher.build_hasher();
        key.hash(&mut s);
        (s.finish() as usize) % self.num_shards
    }
}

#[cfg(test)]
mod tests {
    use super::LruCache;
    use bytes::Bytes;

    #[test]
    fn test_cache_basic() {
        let mut cache = LruCache::new(2);
        cache.put(Bytes::from(vec![1]), Bytes::from(vec![10]));
        cache.put(Bytes::from(vec![2]), Bytes::from(vec![20]));

        assert_eq!(
            cache.get(&Bytes::from(vec![1])),
            Some(Bytes::from(vec![10]))
        );
        assert_eq!(
            cache.get(&Bytes::from(vec![2])),
            Some(Bytes::from(vec![20]))
        );
        assert!(cache.get(&Bytes::from(vec![3])).is_none());
    }

    #[test]
    fn test_cache_eviction() {
        let mut cache = LruCache::new(2);
        cache.put(Bytes::from(vec![1]), Bytes::from(vec![10]));
        cache.put(Bytes::from(vec![2]), Bytes::from(vec![20]));
        // Access 1 to make it MRU
        cache.get(&Bytes::from(vec![1]));

        // Add 3, should evict 2 (since 1 was just accessed)
        cache.put(Bytes::from(vec![3]), Bytes::from(vec![30]));

        assert_eq!(
            cache.get(&Bytes::from(vec![1])),
            Some(Bytes::from(vec![10]))
        );
        assert!(cache.get(&Bytes::from(vec![2])).is_none());
        assert_eq!(
            cache.get(&Bytes::from(vec![3])),
            Some(Bytes::from(vec![30]))
        );
    }

    #[test]
    fn test_cache_update() {
        let mut cache = LruCache::new(2);
        cache.put(Bytes::from(vec![1]), Bytes::from(vec![10]));
        cache.put(Bytes::from(vec![1]), Bytes::from(vec![11]));

        assert_eq!(
            cache.get(&Bytes::from(vec![1])),
            Some(Bytes::from(vec![11]))
        );
    }

    #[test]
    fn test_alloc_handling() {
        let mut cache = LruCache::new(2);
        cache.put(Bytes::from(vec![1]), Bytes::from(vec![10]));
        cache.put(Bytes::from(vec![2]), Bytes::from(vec![20]));
        cache.put(Bytes::from(vec![3]), Bytes::from(vec![30])); // evicts 1

        // Internal check: cache.free should be empty after reuse, but let's just check behavior
        assert_eq!(
            cache.get(&Bytes::from(vec![2])),
            Some(Bytes::from(vec![20]))
        );
        assert_eq!(
            cache.get(&Bytes::from(vec![3])),
            Some(Bytes::from(vec![30]))
        );

        cache.put(Bytes::from(vec![4]), Bytes::from(vec![40])); // evicts 2
        assert!(cache.get(&Bytes::from(vec![2])).is_none());
        assert_eq!(
            cache.get(&Bytes::from(vec![4])),
            Some(Bytes::from(vec![40]))
        );
    }

    #[test]
    fn test_sharded_cache_basic() {
        use super::ShardedCache;
        let cache = ShardedCache::new(10, 2); // 10 capacity, 2 shards
        cache.put(Bytes::from(vec![1]), Bytes::from(vec![10]));
        cache.put(Bytes::from(vec![2]), Bytes::from(vec![20]));

        assert_eq!(
            cache.get(&Bytes::from(vec![1])),
            Some(Bytes::from(vec![10]))
        );
        assert_eq!(
            cache.get(&Bytes::from(vec![2])),
            Some(Bytes::from(vec![20]))
        );
    }
}