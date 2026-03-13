use lru::LruCache;
use std::num::NonZeroUsize;
use once_cell::sync::Lazy;
use dashmap::DashMap;

pub static INSCRIPTION_CACHE: Lazy<DashMap<String, serde_json::Value>> = Lazy::new(|| {
    DashMap::with_capacity(50000)
});
