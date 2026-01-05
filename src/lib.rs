use std::collections::HashMap;
use bytes::Bytes;
use std::fmt::Display;
use std::sync::{Arc, Mutex, Weak};

type Key = Bytes;
type Value = Bytes;
type Index = usize;

#[derive(Debug)]
struct Entry {
    key: Key,
    value: Value,
    prev: Option<Index>,
    next: Option<Index>,
}

#[derive(Debug)]
pub struct Cache {
    capacity: usize,
    map: HashMap<Key, Index>,
    entries: Vec<Entry>,
    head: Option<Index>,
    tail: Option<Index>,
    free: Vec<Index>, // reuse slot

}

impl Cache {
    pub fn new(capacity: usize) -> Self {
        Cache {
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
        Some(self.entries[idx].value.clone()) // O(1)
    }

    pub fn put(&mut self, key: Key, value: Value) {
        if let Some(&idx) = self.map.get(&key) {
            self.entries[idx].value = value;
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
        self.entries[idx].prev = None;
        self.entries[idx].next = self.head;

        if let Some(old_head) = self.head {
            self.entries[old_head].prev = Some(idx);
        } else {
            self.tail = Some(idx);
        }

        self.head = Some(idx);
    }

    fn detach(&mut self, idx: Index) {
        let (prev, next) = {
            let entry = &self.entries[idx];
            (entry.prev, entry.next)
        };

        if let Some(prev_idx) = prev {
            self.entries[prev_idx].next = next;
        } else {
            self.head = next;
        }

        if let Some(next_idx) = next {
            self.entries[next_idx].prev = prev;
        } else {
            self.tail = prev;
        }

        self.entries[idx].prev = None;
        self.entries[idx].next = None;
    }

    fn alloc(&mut self, entry: Entry) -> Index {
        if let Some(idx) = self.free.pop() {
            self.entries[idx] = entry;
            idx
        } else {
            let idx = self.entries.len();
            self.entries.push(entry);
            idx
        }
    }

    fn evict(&mut self) {
        if let Some(tail_idx) = self.tail {
            let key = self.entries[tail_idx].key.clone();
            self.detach(tail_idx);
            self.map.remove(&key);
            self.free.push(tail_idx);
        }
    }
}

impl Display for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut idx = self.head;
        write!(f, "Cache [")?;
        while let Some(i) = idx {
            let entry = &self.entries[i];
            write!(f, "({:?}: {:?}) ", entry.key, entry.value)?;
            idx = entry.next;
        }
        write!(f, "]")
    }
}

#[cfg(test)]
mod tests {
    use super::Cache;

    #[tokio::test]
    async fn test_cache_basic() {
        let mut cache = Cache::new(2);
        cache.push(vec![1], vec![10]);
        cache.push(vec![2], vec![20]);
    }

    #[tokio::test]
    async fn test_cache_eviction() {
        let mut cache = Cache::new(2);
        cache.push(vec![1], vec![10]);
        cache.push(vec![2], vec![20]);
        cache.push(vec![3], vec![30]); // This should evict key [1]

        assert!(cache.get(&vec![1]).is_none());
        assert_eq!(cache.get(&vec![2]), Some(vec![20]));
        assert_eq!(cache.get(&vec![3]), Some(vec![30]));
    }
}
