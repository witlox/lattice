//! LRU image cache for uenv SquashFS images and OCI container images.
//!
//! Caches pulled images on node-local NVMe to avoid re-downloading.
//! Evicts least-recently-used entries when the cache exceeds capacity.

use std::collections::HashMap;

/// Metadata for a cached image.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    /// Image reference (e.g., "prgenv-gnu/24.11:v1" or "nvcr.io/nvidia/pytorch:24.01").
    pub image_ref: String,
    /// Size of the cached image in bytes.
    pub size_bytes: u64,
    /// Monotonically increasing counter for LRU tracking.
    access_order: u64,
}

/// An LRU cache for container/uenv images.
///
/// Tracks images by reference string. When adding a new image would exceed
/// `max_bytes`, the least-recently-used entries are evicted until there
/// is enough space.
#[derive(Debug)]
pub struct ImageCache {
    entries: HashMap<String, CacheEntry>,
    max_bytes: u64,
    current_bytes: u64,
    access_counter: u64,
}

impl ImageCache {
    /// Create a new image cache with the given maximum capacity in bytes.
    pub fn new(max_bytes: u64) -> Self {
        Self {
            entries: HashMap::new(),
            max_bytes,
            current_bytes: 0,
            access_counter: 0,
        }
    }

    /// Check if an image is already cached.
    pub fn contains(&self, image_ref: &str) -> bool {
        self.entries.contains_key(image_ref)
    }

    /// Record an access to a cached image (updates LRU order).
    /// Returns true if the image was found (cache hit), false otherwise.
    pub fn touch(&mut self, image_ref: &str) -> bool {
        if let Some(entry) = self.entries.get_mut(image_ref) {
            self.access_counter += 1;
            entry.access_order = self.access_counter;
            true
        } else {
            false
        }
    }

    /// Add an image to the cache. Evicts LRU entries if needed.
    /// Returns the list of evicted image references.
    pub fn insert(&mut self, image_ref: String, size_bytes: u64) -> Vec<String> {
        // If already cached, just touch it
        if self.entries.contains_key(&image_ref) {
            self.touch(&image_ref);
            return Vec::new();
        }

        // If the image is larger than max capacity, we can't cache it
        if size_bytes > self.max_bytes {
            return Vec::new();
        }

        // Evict until we have enough space
        let mut evicted = Vec::new();
        while self.current_bytes + size_bytes > self.max_bytes {
            if let Some(lru_ref) = self.find_lru() {
                let lru_ref = lru_ref.clone();
                if let Some(entry) = self.entries.remove(&lru_ref) {
                    self.current_bytes -= entry.size_bytes;
                    evicted.push(lru_ref);
                }
            } else {
                break;
            }
        }

        self.access_counter += 1;
        let entry = CacheEntry {
            image_ref: image_ref.clone(),
            size_bytes,
            access_order: self.access_counter,
        };
        self.current_bytes += size_bytes;
        self.entries.insert(image_ref, entry);

        evicted
    }

    /// Remove an image from the cache.
    pub fn remove(&mut self, image_ref: &str) -> bool {
        if let Some(entry) = self.entries.remove(image_ref) {
            self.current_bytes -= entry.size_bytes;
            true
        } else {
            false
        }
    }

    /// Current total cached size in bytes.
    pub fn current_bytes(&self) -> u64 {
        self.current_bytes
    }

    /// Maximum cache capacity in bytes.
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Number of cached images.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Find the least-recently-used entry.
    fn find_lru(&self) -> Option<&String> {
        self.entries
            .iter()
            .min_by_key(|(_, e)| e.access_order)
            .map(|(k, _)| k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cache() {
        let cache = ImageCache::new(1024);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.current_bytes(), 0);
        assert!(!cache.contains("foo"));
    }

    #[test]
    fn insert_and_contains() {
        let mut cache = ImageCache::new(1024);
        let evicted = cache.insert("image-a".to_string(), 100);
        assert!(evicted.is_empty());
        assert!(cache.contains("image-a"));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.current_bytes(), 100);
    }

    #[test]
    fn insert_duplicate_is_noop() {
        let mut cache = ImageCache::new(1024);
        cache.insert("image-a".to_string(), 100);
        let evicted = cache.insert("image-a".to_string(), 100);
        assert!(evicted.is_empty());
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.current_bytes(), 100);
    }

    #[test]
    fn eviction_removes_lru() {
        let mut cache = ImageCache::new(250);
        cache.insert("image-a".to_string(), 100);
        cache.insert("image-b".to_string(), 100);

        // This should evict image-a (oldest)
        let evicted = cache.insert("image-c".to_string(), 100);
        assert_eq!(evicted, vec!["image-a"]);
        assert!(!cache.contains("image-a"));
        assert!(cache.contains("image-b"));
        assert!(cache.contains("image-c"));
        assert_eq!(cache.current_bytes(), 200);
    }

    #[test]
    fn touch_updates_lru_order() {
        let mut cache = ImageCache::new(250);
        cache.insert("image-a".to_string(), 100);
        cache.insert("image-b".to_string(), 100);

        // Touch image-a to make it more recent
        assert!(cache.touch("image-a"));

        // Now image-b should be evicted (oldest after touch)
        let evicted = cache.insert("image-c".to_string(), 100);
        assert_eq!(evicted, vec!["image-b"]);
        assert!(cache.contains("image-a"));
        assert!(cache.contains("image-c"));
    }

    #[test]
    fn touch_miss_returns_false() {
        let mut cache = ImageCache::new(1024);
        assert!(!cache.touch("nonexistent"));
    }

    #[test]
    fn evict_multiple() {
        let mut cache = ImageCache::new(300);
        cache.insert("a".to_string(), 100);
        cache.insert("b".to_string(), 100);
        cache.insert("c".to_string(), 100);

        // Need to evict a and b to fit d (size 200)
        let evicted = cache.insert("d".to_string(), 200);
        assert_eq!(evicted.len(), 2);
        assert!(evicted.contains(&"a".to_string()));
        assert!(evicted.contains(&"b".to_string()));
        assert!(cache.contains("c"));
        assert!(cache.contains("d"));
    }

    #[test]
    fn image_too_large_not_cached() {
        let mut cache = ImageCache::new(100);
        let evicted = cache.insert("huge".to_string(), 200);
        assert!(evicted.is_empty());
        assert!(!cache.contains("huge"));
        assert_eq!(cache.current_bytes(), 0);
    }

    #[test]
    fn remove_entry() {
        let mut cache = ImageCache::new(1024);
        cache.insert("image-a".to_string(), 100);
        assert!(cache.remove("image-a"));
        assert!(!cache.contains("image-a"));
        assert_eq!(cache.current_bytes(), 0);
    }

    #[test]
    fn remove_nonexistent() {
        let mut cache = ImageCache::new(1024);
        assert!(!cache.remove("nonexistent"));
    }

    #[test]
    fn max_bytes_accessor() {
        let cache = ImageCache::new(4096);
        assert_eq!(cache.max_bytes(), 4096);
    }
}
