//! Request-owned work that cannot be stopped by dropping its async waiter.
use std::future::Future;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::{sync::Notify, task::JoinHandle};

tokio::task_local! {
    static REQUEST_WORK: BlockingWorkTracker;
}

#[derive(Clone, Default)]
pub struct BlockingWorkTracker {
    state: Arc<State>,
}

#[derive(Default)]
struct State {
    active: AtomicUsize,
    idle: Notify,
}

impl BlockingWorkTracker {
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        REQUEST_WORK.scope(self.clone(), future).await
    }

    pub fn is_idle(&self) -> bool {
        self.state.active.load(Ordering::Acquire) == 0
    }

    pub async fn wait_idle(&self) {
        loop {
            // Register before observing the count: completion between a count
            // check and listener registration must not strand the FIFO.
            let notified = self.state.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_idle() {
                return;
            }
            notified.await;
        }
    }

    fn start(&self) -> BlockingWorkGuard {
        self.state.active.fetch_add(1, Ordering::AcqRel);
        BlockingWorkGuard {
            state: self.state.clone(),
        }
    }
}

pub(crate) struct BlockingWorkGuard {
    state: Arc<State>,
}

impl Drop for BlockingWorkGuard {
    fn drop(&mut self) {
        if self.state.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.state.idle.notify_waiters();
        }
    }
}

pub(crate) fn track_current_work() -> Option<BlockingWorkGuard> {
    REQUEST_WORK.try_with(BlockingWorkTracker::start).ok()
}

/// Register before scheduling, including work still queued for a worker. The
/// guard is owned by the closure, never by its cancellable JoinHandle waiter.
pub(crate) fn spawn_tracked_blocking<F, R>(work: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let guard = track_current_work();
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        work()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn worker_panic_releases_the_request_barrier() {
        let tracker = BlockingWorkTracker::default();
        let (worker,) = tracker
            .scope(async { (spawn_tracked_blocking(|| panic!("fixture worker panic")),) })
            .await;
        assert!(worker.await.unwrap_err().is_panic());
        timeout(Duration::from_secs(1), tracker.wait_idle())
            .await
            .unwrap();
        assert!(tracker.is_idle());
    }

    #[tokio::test]
    async fn guard_is_owned_before_worker_scheduling_and_drop_wakes_waiter() {
        let tracker = BlockingWorkTracker::default();
        let guard = tracker.start();
        assert!(!tracker.is_idle());
        let waiting = tracker.clone();
        let waiter = tokio::spawn(async move { waiting.wait_idle().await });
        drop(guard);
        timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        assert!(tracker.is_idle());
    }

    #[tokio::test]
    async fn work_outside_a_request_scope_retains_existing_behavior() {
        assert!(track_current_work().is_none());
        assert_eq!(spawn_tracked_blocking(|| 7).await.unwrap(), 7);
    }
    #[tokio::test]
    async fn a_detached_worker_retains_ownership_after_its_scope_returns() {
        let tracker = BlockingWorkTracker::default();
        let (release, receive) = std::sync::mpsc::channel();
        let (worker,) = tracker
            .scope(async { (spawn_tracked_blocking(move || receive.recv().unwrap()),) })
            .await;
        drop(worker);
        let retained = !tracker.is_idle();
        release.send(()).unwrap();
        timeout(Duration::from_secs(1), tracker.wait_idle())
            .await
            .unwrap();
        assert!(retained);
    }

    #[test]
    fn core_workers_cannot_bypass_request_ownership_tracking() {
        fn visit(path: &std::path::Path, offenders: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, offenders);
                } else if path.extension().is_some_and(|extension| extension == "rs")
                    && path
                        .file_name()
                        .is_none_or(|name| name != "blocking_work.rs")
                    && std::fs::read_to_string(&path)
                        .unwrap()
                        .contains("spawn_blocking(")
                {
                    offenders.push(path);
                }
            }
        }
        let mut offenders = Vec::new();
        visit(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut offenders,
        );
        assert!(
            offenders.is_empty(),
            "untracked blocking workers: {offenders:?}"
        );
    }
}
