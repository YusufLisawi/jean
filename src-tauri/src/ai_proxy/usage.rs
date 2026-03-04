//! Simple in-memory usage tracking with atomic counters.
//!
//! Tracks request counts per model and per provider. All operations are
//! thread-safe via atomics and `RwLock`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::SystemTime;

use super::types::UsageStats;

pub struct UsageTracker {
    started_at: SystemTime,
    total_requests: AtomicU64,
    per_model: RwLock<HashMap<String, u64>>,
    per_provider: RwLock<HashMap<String, u64>>,
}

impl UsageTracker {
    pub fn new() -> Self {
        Self {
            started_at: SystemTime::now(),
            total_requests: AtomicU64::new(0),
            per_model: RwLock::new(HashMap::new()),
            per_provider: RwLock::new(HashMap::new()),
        }
    }

    /// Record a single request for a model/provider pair.
    pub fn record(&self, model: &str, provider: &str) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);

        if let Ok(mut map) = self.per_model.write() {
            *map.entry(model.to_string()).or_default() += 1;
        }
        if let Ok(mut map) = self.per_provider.write() {
            *map.entry(provider.to_string()).or_default() += 1;
        }
    }

    /// Snapshot current usage statistics.
    pub fn stats(&self) -> UsageStats {
        UsageStats {
            total_requests: self.total_requests.load(Ordering::Relaxed),
            per_model: self.per_model.read().map(|m| m.clone()).unwrap_or_default(),
            per_provider: self
                .per_provider
                .read()
                .map(|m| m.clone())
                .unwrap_or_default(),
            per_account: HashMap::new(),
            by_request: Vec::new(),
        }
    }

    /// Reset all counters.
    pub fn reset(&self) {
        self.total_requests.store(0, Ordering::Relaxed);
        if let Ok(mut map) = self.per_model.write() {
            map.clear();
        }
        if let Ok(mut map) = self.per_provider.write() {
            map.clear();
        }
    }

    /// Time since tracker was created.
    pub fn uptime(&self) -> std::time::Duration {
        self.started_at.elapsed().unwrap_or_default()
    }
}

impl Default for UsageTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_stats() {
        let tracker = UsageTracker::new();
        tracker.record("claude-sonnet-4", "claude");
        tracker.record("claude-sonnet-4", "claude");
        tracker.record("gpt-4o", "openai");

        let stats = tracker.stats();
        assert_eq!(stats.total_requests, 3);
        assert_eq!(stats.per_model["claude-sonnet-4"], 2);
        assert_eq!(stats.per_model["gpt-4o"], 1);
        assert_eq!(stats.per_provider["claude"], 2);
        assert_eq!(stats.per_provider["openai"], 1);
    }

    #[test]
    fn reset_clears_all() {
        let tracker = UsageTracker::new();
        tracker.record("model-a", "provider-a");
        tracker.reset();

        let stats = tracker.stats();
        assert_eq!(stats.total_requests, 0);
        assert!(stats.per_model.is_empty());
        assert!(stats.per_provider.is_empty());
    }

    #[test]
    fn default_starts_empty() {
        let stats = UsageTracker::default().stats();
        assert_eq!(stats.total_requests, 0);
    }
}
