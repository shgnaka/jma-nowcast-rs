//! Memory cache for decoded JMA hrpns tiles (R5).

use crate::tiles::TileCoord;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Key for the tile cache: basetime, validtime, and tile XYZ coordinate.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TileCacheKey {
    pub basetime: DateTime<Utc>,
    pub validtime: DateTime<Utc>,
    pub tile: TileCoord,
}

/// Thread-safe bounded in-memory tile cache.
/// Evicts arbitrary entries or oldest when capacity is reached.
#[derive(Clone)]
pub struct TileCache {
    capacity: usize,
    entries: Arc<Mutex<HashMap<TileCacheKey, Arc<Vec<u8>>>>>,
}

impl TileCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Arc::new(Mutex::new(HashMap::with_capacity(capacity))),
        }
    }

    pub fn get(&self, key: &TileCacheKey) -> Option<Arc<Vec<u8>>> {
        if self.capacity == 0 {
            return None;
        }
        let guard = self.entries.lock().unwrap();
        guard.get(key).cloned()
    }

    pub fn insert(&self, key: TileCacheKey, decoded: Arc<Vec<u8>>) {
        if self.capacity == 0 {
            return;
        }
        let mut guard = self.entries.lock().unwrap();
        if guard.len() >= self.capacity && !guard.contains_key(&key) {
            // Evict one entry if at capacity
            if let Some(first_key) = guard.keys().next().cloned() {
                guard.remove(&first_key);
            }
        }
        guard.insert(key, decoded);
    }
}
