use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, Mutex, Weak};

type Link = Option<Arc<Mutex<Node>>>;
type WeakLink = Option<Weak<Mutex<Node>>>;

#[derive(Debug)]
struct Node {
    key: Vec<u8>, // Stored to allow removal from HashMap during eviction
    value: Vec<u8>,
    prev: WeakLink, // use Weak to avoid reference cycles (memory leaks)
    next: Link,
}

#[derive(Debug, Clone)]
pub struct Cache {
    capacity: usize,
    head: Link,
    tail: Link,
    map: HashMap<Vec<u8>, Arc<Mutex<Node>>>,
}

impl Cache {
    pub fn new(capacity: usize) -> Self {
        Cache {
            capacity,
            head: None,
            tail: None,
            map: HashMap::new(),
        }
    }

    pub fn get(&mut self, key: &Vec<u8>) -> Option<Vec<u8>> {
        // Clone the Rc to drop the map borrow immediately
        let node = self.map.get(key)?.clone();

        // Move to front because it was recently used
        self.detach_node(node.clone());
        self.attach_to_head(node.clone());

        let val = node.lock().unwrap().value.clone();
        Some(val)
    }

    pub fn push(&mut self, key: Vec<u8>, value: Vec<u8>) {
        if let Some(node) = self.map.get(&key).cloned() {
            // Update existing
            node.lock().unwrap().value = value;
            self.detach_node(node.clone());
            self.attach_to_head(node);
        } else {
            // Check capacity
            if self.map.len() >= self.capacity {
                self.evict();
            }

            // Create new
            let new_node = Arc::new(Mutex::new(Node {
                key: key.clone(),
                value,
                prev: None,
                next: None,
            }));

            self.map.insert(key, new_node.clone());
            self.attach_to_head(new_node);
        }
    }

    // --- Helper Methods ---

    fn detach_node(&mut self, node: Arc<Mutex<Node>>) {
        let (prev_weak, next_rc) = {
            let mut n = node.lock().unwrap();
            (n.prev.take(), n.next.take())
        };

        // Fix the 'next' pointer of the previous node
        if let Some(ref p_weak) = prev_weak {
            if let Some(p_arc) = p_weak.upgrade() {
                p_arc.lock().unwrap().next = next_rc.clone();
            }
        } else {
            self.head = next_rc.clone();
        }

        // Fix the 'prev' pointer of the next node
        if let Some(ref n_arc) = next_rc {
            n_arc.lock().unwrap().prev = prev_weak.clone();
        } else {
            self.tail = prev_weak.and_then(|w| w.upgrade());
        }
    }

    fn attach_to_head(&mut self, node: Arc<Mutex<Node>>) {
        let mut n = node.lock().unwrap();
        n.next = self.head.clone();
        n.prev = None;

        if let Some(ref old_head) = self.head {
            old_head.lock().unwrap().prev = Some(Arc::downgrade(&node));
        } else {
            self.tail = Some(node.clone());
        }
        drop(n); // Explicitly drop lock before modifying self.head

        self.head = Some(node);
    }

    fn evict(&mut self) {
        if let Some(old_tail) = self.tail.clone() {
            self.detach_node(old_tail.clone());
            let key = &old_tail.lock().unwrap().key;
            self.map.remove(key);
        }
    }
}

impl Display for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut node_opt = self.head.clone();
        let mut first = true;
        writeln!(f, "Cache [")?;
        while let Some(node_rc) = node_opt {
            if !first {
                writeln!(f, " <-> ")?;
            }
            let node = node_rc.lock().unwrap();
            writeln!(f, "({:?}: {:?})", node.key, node.value)?;
            node_opt = node.next.clone();
            first = false;
        }
        writeln!(f, "]")
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
