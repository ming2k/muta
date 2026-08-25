//! Inflight request deduplication map (inspired by Google Cider / Jetski `inflight.Map`).
//!
//! When multiple concurrent tasks request the same key `K` (such as scanning
//! the same file, calculating AST symbols, or checking remote endpoints),
//! `Inflight` ensures that only one task actually executes the underlying
//! async operation. All other concurrent callers await the shared in-flight result.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

/// Inflight request manager for deduplicating concurrent operations by key.
#[derive(Clone)]
pub struct Inflight<K, V> {
    tasks: Arc<Mutex<HashMap<K, broadcast::Sender<V>>>>,
}

impl<K, V> Default for Inflight<K, V>
where
    K: Eq + Hash + Clone + Send + 'static,
    V: Clone + Send + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> Inflight<K, V>
where
    K: Eq + Hash + Clone + Send + 'static,
    V: Clone + Send + 'static,
{
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get the result from an existing in-flight task, or compute it using `f`.
    ///
    /// If an operation for `key` is already running, this caller attaches to its
    /// broadcast channel and waits for completion. If no operation is running,
    /// this caller spawns `f()`, broadcasts the result to all waiters, and cleans up.
    pub async fn get_or_compute<F, Fut>(&self, key: K, f: F) -> Result<V, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<V, String>>,
    {
        let mut rx = {
            let mut lock = self.tasks.lock().await;
            if let Some(sender) = lock.get(&key) {
                // Another task is already computing this key; subscribe to it.
                sender.subscribe()
            } else {
                // We are the initiator. Create a broadcast channel.
                let (tx, _rx) = broadcast::channel(1);
                lock.insert(key.clone(), tx.clone());
                drop(lock);

                // Run computation
                let result = f().await;

                // Clean up inflight map
                let mut lock = self.tasks.lock().await;
                lock.remove(&key);
                drop(lock);

                match result {
                    Ok(val) => {
                        let _ = tx.send(val.clone());
                        return Ok(val);
                    }
                    Err(err) => {
                        return Err(err);
                    }
                }
            }
        };

        // Wait for initiator's broadcast
        rx.recv()
            .await
            .map_err(|e| format!("Inflight request cancelled or dropped: {}", e))
    }

    /// Check the current number of in-flight operations.
    pub async fn len(&self) -> usize {
        self.tasks.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.tasks.lock().await.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::time::sleep;

    #[tokio::test]
    async fn deduplicates_concurrent_calls_for_same_key() {
        let inflight = Inflight::<String, usize>::new();
        let counter = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..10 {
            let inf = inflight.clone();
            let cnt = counter.clone();
            handles.push(tokio::spawn(async move {
                inf.get_or_compute("task_key".to_string(), || async {
                    sleep(Duration::from_millis(50)).await;
                    Ok(cnt.fetch_add(1, Ordering::SeqCst) + 42)
                })
                .await
            }));
        }

        for h in handles {
            let res = h.await.unwrap();
            assert_eq!(res.unwrap(), 42);
        }

        // The computation must have executed exactly ONCE
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(inflight.len().await, 0);
    }
}
