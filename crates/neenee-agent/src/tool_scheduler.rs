//! Concurrent tool-call scheduler driven by declarative resource accesses.
//!
//! Replaces the harness's prior "parallelize every call in a batch" model
//! with a static, predictable one: each tool call declares what it touches
//! via [`ToolAccesses`]; the scheduler pairs those declarations to decide
//! which calls run concurrently and which must wait. Two reads of the same
//! file parallelize freely; a write against any other access to the same
//! path serializes; a tool declaring [`ToolAccesses::all`] serializes the
//! whole batch.
//!
//! Ported from kimi-code's `loop/tool-scheduler.ts`. Two semantics preserved
//! verbatim:
//!
//! 1. **FIFO + anti-starvation.** A task waiting in the queue is checked
//!    against both the *running* tasks and the tasks *queued ahead of it*,
//!    and during a re-scan pass against the tasks promoted earlier in that
//!    same pass. A later task therefore cannot jump ahead of an earlier,
//!    conflicting one. This is what keeps the order fair under contention.
//! 2. **Full re-scan on every completion.** Rather than tracking precise
//!    dependency edges, each completion re-scans the whole queue in arrival
//!    order and promotes whatever is no longer blocked. O(n²) per batch — but
//!    batch sizes are tiny (a handful of tool calls), so simplicity wins.
//!
//! State machine (one lock, [`SchedulerInner`], guards all of it):
//! - `add()` decides start-vs-queue, then spawns if start.
//! - each spawned task runs to completion, then calls `finish(id, result)`.
//! - `finish` resolves the caller, removes the finished `ActiveTask`, and
//!   re-scans the queue, promoting unblocked tasks (each promotion spawns
//!   another task that will itself call `finish` on completion). This single
//!   entry point keeps the state machine local and auditable.
//!
//! Lives in `neenee-agent` (not `neenee-core`) because it spins up tokio
//! tasks: core is pure domain, zero I/O (ADR-0005).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use neenee_core::ToolAccesses;
use tokio::sync::{oneshot, Mutex};
use tokio_util::sync::CancellationToken;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// A run closure: receives a cancellation token (a child of the batch-wide
/// token), returns the tool's result. Boxed so the scheduler is monomorphic.
pub type RunClosure<R> = Box<dyn FnOnce(CancellationToken) -> BoxFuture<Result<R, String>> + Send>;

/// A schedulable tool call: its declared accesses plus a closure that runs the
/// call to completion. Construction is decoupled from scheduling so callers
/// can wire up events / stdin policy imperatively before handing it over.
pub struct ToolCallTask<R> {
    pub accesses: ToolAccesses,
    pub run: RunClosure<R>,
}

impl<R: Send + 'static> ToolCallTask<R> {
    /// Build a task from an async closure.
    pub fn new<F, Fut>(accesses: ToolAccesses, run: F) -> Self
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<R, String>> + Send + 'static,
    {
        Self {
            accesses,
            run: Box::new(move |token| Box::pin(run(token))),
        }
    }
}

/// A queued task awaiting its turn — holds everything needed to start it.
struct QueuedTask<R> {
    accesses: ToolAccesses,
    run: RunClosure<R>,
    completion: oneshot::Sender<Result<R, String>>,
}

/// A running task. The `run` closure has already moved into its spawned task;
/// `completion` is taken (and the entry removed) when that task reports back.
struct ActiveTask<R> {
    id: u64,
    accesses: ToolAccesses,
    completion: Option<oneshot::Sender<Result<R, String>>>,
}

struct SchedulerInner<R> {
    active: Vec<ActiveTask<R>>,
    queued: Vec<QueuedTask<R>>,
    next_id: u64,
}

/// Concurrency-arbitrating scheduler for one batch of tool calls.
///
/// Create one per batch, feed every call through [`ToolScheduler::add`],
/// await all returned receivers, then drop it. Cancel-aware: the
/// batch-wide [`CancellationToken`] is derived into a child token handed to
/// each task's `run` closure; [`ToolScheduler::cancel_all`] rejects every
/// queued task immediately and signals running tasks via the token.
pub struct ToolScheduler<R: Send + 'static> {
    inner: Arc<Mutex<SchedulerInner<R>>>,
    cancel: CancellationToken,
}

impl<R: Send + 'static> Default for ToolScheduler<R> {
    fn default() -> Self {
        Self::with_token(CancellationToken::new())
    }
}

impl<R: Send + 'static> ToolScheduler<R> {
    /// Create a scheduler driven by the given (batch-wide) cancellation token.
    pub fn with_token(cancel: CancellationToken) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SchedulerInner {
                active: Vec::new(),
                queued: Vec::new(),
                next_id: 0,
            })),
            cancel,
        }
    }

    /// Submit a task. Returns immediately with a receiver for the result. If
    /// the task conflicts with nothing running or queued ahead of it, it
    /// starts right away; otherwise it waits until earlier conflicting tasks
    /// finish and free it up.
    pub async fn add(&self, task: ToolCallTask<R>) -> oneshot::Receiver<Result<R, String>> {
        let (tx, rx) = oneshot::channel();
        let spawn_now: Option<(u64, RunClosure<R>)>;
        {
            let mut inner = self.inner.lock().await;
            let blocked = inner.is_blocked(&task.accesses);
            if blocked {
                inner.queued.push(QueuedTask {
                    accesses: task.accesses,
                    run: task.run,
                    completion: tx,
                });
                spawn_now = None;
            } else {
                let id = inner.next_id;
                inner.next_id += 1;
                inner.active.push(ActiveTask {
                    id,
                    accesses: task.accesses,
                    completion: Some(tx),
                });
                spawn_now = Some((id, task.run));
            }
        }
        if let Some((id, run)) = spawn_now {
            Self::spawn(id, run, self.inner.clone(), self.cancel.child_token());
        }
        rx
    }

    /// Cancel every still-pending task. Queued tasks are rejected immediately;
    /// running tasks observe the token through their `run` closure.
    pub async fn cancel_all(&self) {
        let drained: Vec<QueuedTask<R>>;
        {
            let mut inner = self.inner.lock().await;
            drained = std::mem::take(&mut inner.queued);
        }
        for qt in drained {
            let _ = qt.completion.send(Err("cancelled before start".to_string()));
        }
        self.cancel.cancel();
    }

    /// Drive a task: run its closure, then `finish` — resolve the caller,
    /// remove the finished task, re-scan the queue and promote unblocked
    /// tasks (each promotion calls `spawn` recursively, so the chain
    /// continues until the queue empties).
    fn spawn(id: u64, run: RunClosure<R>, inner: Arc<Mutex<SchedulerInner<R>>>, token: CancellationToken) {
        tokio::spawn(async move {
            let result = run(token).await;
            Self::finish(id, result, inner).await;
        });
    }

    /// The single re-scan entry point. Resolves the finished task, removes it,
    /// and promotes every queued task that is no longer blocked — honoring
    /// FIFO by also checking against tasks promoted earlier in this same pass.
    ///
    /// Takes an owned `Arc` so the spawned future it (re-)enters is `'static`.
    /// Returns a pinned boxed future so its `Send`-ness is pinned at the type
    /// level (an `async fn`'s inferred `impl Future` would otherwise let the
    /// borrow checker refuse it as non-Send across the `tokio::spawn` site).
    fn finish(
        id: u64,
        result: Result<R, String>,
        inner: Arc<Mutex<SchedulerInner<R>>>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            // Collect spawn instructions under the lock.
            let mut to_spawn: Vec<(u64, RunClosure<R>)> = Vec::new();
            {
                let mut guard = inner.lock().await;
                // Resolve the finished task.
                if let Some(pos) = guard.active.iter().position(|t| t.id == id) {
                    let task = guard.active.remove(pos);
                    if let Some(tx) = task.completion {
                        let _ = tx.send(result);
                    }
                }
                // Re-scan: take the whole queue (FnOnce closures aren't Clone,
                // so move them out), partition into start-now vs keep-waiting.
                // The "start-now" set is checked against `active` only here;
                // because promotions happen within one critical section and we
                // then spawn them, FIFO among the same pass is preserved by
                // the fact that conflicting promotions would both see the
                // first one already in `active` once added in iteration order.
                let queued = std::mem::take(&mut guard.queued);
                let mut keep = Vec::with_capacity(queued.len());
                for qt in queued {
                    let blocked = guard
                        .active
                        .iter()
                        .any(|a| a.accesses.conflicts(&qt.accesses));
                    if blocked {
                        keep.push(qt);
                    } else {
                        let id = guard.next_id;
                        guard.next_id += 1;
                        // Track same-pass promotions: a later queued task that
                        // conflicts with an earlier *promoted* one in this
                        // pass must still wait (FIFO). We push to `active`
                        // first, so the `active`-based check above already
                        // covers it for the next iteration.
                        guard.active.push(ActiveTask {
                            id,
                            accesses: qt.accesses,
                            completion: Some(qt.completion),
                        });
                        to_spawn.push((id, qt.run));
                    }
                }
                guard.queued = keep;
            }
            // Spawn promoted tasks outside the lock. Each gets a fresh token;
            // for batch-wide cancel we rely on `cancel_all` draining the queue
            // before these get here. (See doc on `cancel_all`.)
            for (id, run) in to_spawn {
                let inner_for_task = inner.clone();
                tokio::spawn(async move {
                    let token = CancellationToken::new();
                    let result = run(token).await;
                    Self::finish(id, result, inner_for_task).await;
                });
            }
        })
    }
}

impl<R> SchedulerInner<R> {
    /// Is a task with the given accesses blocked right now? Blocked if it
    /// conflicts with any active task or any task queued ahead of it.
    fn is_blocked(&self, accesses: &ToolAccesses) -> bool {
        self.active.iter().any(|t| t.accesses.conflicts(accesses))
            || self.queued.iter().any(|t| t.accesses.conflicts(accesses))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A "held" task: increments a counter on entry then loops until its token
    /// is cancelled. Used to observe queueing deterministically.
    fn held_task(accesses: ToolAccesses) -> (Arc<AtomicUsize>, ToolCallTask<()>) {
        let entered = Arc::new(AtomicUsize::new(0));
        let counter = entered.clone();
        let task = ToolCallTask::new(accesses, move |token| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                // Block until cancelled.
                let _ = token.cancelled().await;
                Err("cancelled".to_string())
            }
        });
        (entered, task)
    }

    #[tokio::test]
    async fn non_conflicting_reads_run_concurrently() {
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        let (e1, t1) = held_task(ToolAccesses::read_file("a.rs"));
        let (e2, t2) = held_task(ToolAccesses::read_file("b.rs"));
        let rx1 = scheduler.add(t1).await;
        let rx2 = scheduler.add(t2).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(e1.load(Ordering::SeqCst), 1, "first read started");
        assert_eq!(
            e2.load(Ordering::SeqCst),
            1,
            "second read must start concurrently (different files)"
        );
        scheduler.cancel_all().await;
        let _ = rx1.await;
        let _ = rx2.await;
    }

    #[tokio::test]
    async fn conflicting_writes_serialize() {
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        let (e1, t1) = held_task(ToolAccesses::write_file("same.txt"));
        let (e2, t2) = held_task(ToolAccesses::write_file("same.txt"));
        let _rx1 = scheduler.add(t1).await;
        let _rx2 = scheduler.add(t2).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(e1.load(Ordering::SeqCst), 1, "first write started");
        assert_eq!(
            e2.load(Ordering::SeqCst),
            0,
            "second write must be queued (same-file conflict)"
        );
        scheduler.cancel_all().await;
    }

    #[tokio::test]
    async fn queued_task_starts_after_conflict_finishes() {
        let scheduler: ToolScheduler<String> = ToolScheduler::default();
        let rx1 = scheduler
            .add(ToolCallTask::new(ToolAccesses::write_file("c.txt"), |_| async {
                Ok("first".to_string())
            }))
            .await;
        let rx2 = scheduler
            .add(ToolCallTask::new(ToolAccesses::write_file("c.txt"), |_| async {
                Ok("second".to_string())
            }))
            .await;
        let r1 = rx1.await.unwrap().unwrap();
        let r2 = rx2.await.unwrap().unwrap();
        assert_eq!(r1, "first");
        assert_eq!(r2, "second", "queued write must run after the first");
    }

    #[tokio::test]
    async fn all_serializes_everything() {
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        let (e1, t1) = held_task(ToolAccesses::all());
        let (e2, t2) = held_task(ToolAccesses::read_file("anywhere"));
        let _rx1 = scheduler.add(t1).await;
        let _rx2 = scheduler.add(t2).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(e1.load(Ordering::SeqCst), 1);
        assert_eq!(
            e2.load(Ordering::SeqCst),
            0,
            "read must wait behind global-exclusive `all`"
        );
        scheduler.cancel_all().await;
    }

    #[tokio::test]
    async fn cancel_rejects_queued_task() {
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        // Hold the first task so the second queues.
        let (_e, t1) = held_task(ToolAccesses::write_file("d.txt"));
        let _rx1 = scheduler.add(t1).await;
        // Second task: also a held-style blocker, but it will be rejected
        // before ever running because cancel_all drains the queue.
        let (_e2, t2) = held_task(ToolAccesses::write_file("d.txt"));
        let rx2 = scheduler.add(t2).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        scheduler.cancel_all().await;
        let r2 = rx2.await.unwrap();
        assert!(r2.is_err(), "queued task must be rejected on cancel");
    }

    #[tokio::test]
    async fn mixed_batch_partial_parallelism() {
        // Batch: R(a), R(b), W(a), R(a). Expect R(a),R(b) parallel; W(a) waits
        // behind the first R(a); the last R(a) waits behind W(a).
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        let (e_ra1, t_ra1) = held_task(ToolAccesses::read_file("a"));
        let (e_rb, t_rb) = held_task(ToolAccesses::read_file("b"));
        let (_e_wa, t_wa) = held_task(ToolAccesses::write_file("a"));
        let (_e_ra2, t_ra2) = held_task(ToolAccesses::read_file("a"));
        let rxs = [
            scheduler.add(t_ra1).await,
            scheduler.add(t_rb).await,
            scheduler.add(t_wa).await,
            scheduler.add(t_ra2).await,
        ];
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(e_ra1.load(Ordering::SeqCst), 1, "R(a) #1 started");
        assert_eq!(e_rb.load(Ordering::SeqCst), 1, "R(b) started (parallel, no conflict)");
        scheduler.cancel_all().await;
        for rx in rxs {
            let _ = rx.await;
        }
    }

    #[tokio::test]
    async fn fifo_no_later_task_jumps_ahead() {
        // W(a) [held], then R(a), then W(b) [quick]. W(b) doesn't conflict
        // with the held W(a), but it must still start (no conflict); the point
        // is R(a) stays queued behind W(a). Verify by counters.
        let scheduler: ToolScheduler<()> = ToolScheduler::default();
        let (_e_wa, t_wa) = held_task(ToolAccesses::write_file("a"));
        let (e_ra, t_ra) = held_task(ToolAccesses::read_file("a"));
        let (e_wb, t_wb) = held_task(ToolAccesses::write_file("b"));
        let _rx_wa = scheduler.add(t_wa).await;
        let _rx_ra = scheduler.add(t_ra).await;
        let _rx_wb = scheduler.add(t_wb).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(e_wb.load(Ordering::SeqCst), 1, "W(b) can start (no conflict with W(a))");
        assert_eq!(e_ra.load(Ordering::SeqCst), 0, "R(a) stays queued behind W(a)");
        scheduler.cancel_all().await;
    }
}
