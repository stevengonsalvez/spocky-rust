//! Starts a message handler the way a JavaScript async function starts.
//!
//! Pinned does not queue a socket's messages: `websocket-server.ts:2354`
//! starts each with `void this.dispatchSessionMessage(..)`, so a handler runs
//! synchronously to its first await, in arrival order, and the rest runs
//! concurrently. `tokio::spawn` runs none of a handler before the next
//! message arrives, so a `wait_for_finish` sent right after an
//! `agent_permission_response` could read its agent before the response took
//! effect. [`start_inline`] polls the handler once on the caller's thread, in
//! call order, and hands the rest to the runtime.
//!
//! The caller is the connection's reader thread (`server.rs:1427`), where a
//! panic would close the socket; pinned keeps a throwing handler from closing
//! it, and a panic in a spawned task was contained by the runtime. Every poll
//! catches a panic the same way: the handler is dropped, its request gets no
//! reply, and the connection stays open.

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};

use tokio::runtime::Handle;

struct Task {
    future: Mutex<Option<Pin<Box<dyn Future<Output = ()> + Send>>>>,
    runtime: Handle,
}

impl Task {
    /// Polls the future once. A panic ends the task, whether it comes in the
    /// inline segment or in a later poll on the runtime: the future is
    /// dropped, so a later wake finds nothing to poll, and the lock is never
    /// poisoned.
    fn poll(self: &Arc<Self>) {
        let waker = Waker::from(Arc::clone(self));
        let mut context = Context::from_waker(&waker);
        let mut slot = self.future.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(future) = slot.as_mut() else {
            return;
        };
        // The panic message is already out through the panic hook, as it is
        // for a spawned task.
        match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(&mut context))) {
            Ok(Poll::Pending) => {}
            Ok(Poll::Ready(())) | Err(_) => *slot = None,
        }
    }
}

impl Wake for Task {
    // ponytail: a wake during the inline poll queues a runtime task that waits
    // on the lock until that poll ends; a handler's synchronous prefix is short.
    fn wake(self: Arc<Self>) {
        let runtime = self.runtime.clone();
        runtime.spawn(async move { self.poll() });
    }
}

/// Runs `task` up to its first pending await now, on this thread, with
/// `runtime` entered; the rest runs on `runtime`. A panic in any poll ends the
/// task and does not reach the caller.
pub fn start_inline(runtime: &Handle, task: impl Future<Output = ()> + Send + 'static) {
    let _entered = runtime.enter();
    let task = Arc::new(Task {
        future: Mutex::new(Some(Box::pin(task))),
        runtime: runtime.clone(),
    });
    task.poll();
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::runtime::Handle;
    use tokio::sync::oneshot;

    use super::start_inline;

    fn record(log: &Arc<Mutex<Vec<&'static str>>>, entry: &'static str) {
        log.lock().unwrap().push(entry);
    }

    #[tokio::test]
    async fn each_task_runs_to_its_first_await_in_call_order() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (done_a, finished_a) = oneshot::channel();
        let (done_b, finished_b) = oneshot::channel();
        for (name, rest, done) in [("a1", "a2", done_a), ("b1", "b2", done_b)] {
            let log = Arc::clone(&log);
            start_inline(&Handle::current(), async move {
                record(&log, name);
                tokio::task::yield_now().await;
                record(&log, rest);
                let _ = done.send(());
            });
        }
        // Both prefixes ran before this thread returned to the runtime.
        assert_eq!(*log.lock().unwrap(), ["a1", "b1"]);
        finished_a.await.unwrap();
        finished_b.await.unwrap();
        let mut log = log.lock().unwrap().clone();
        log.sort_unstable();
        assert_eq!(log, ["a1", "a2", "b1", "b2"]);
    }

    #[tokio::test]
    async fn a_pending_task_resumes_when_woken_elsewhere() {
        let (wake, woken) = oneshot::channel::<()>();
        let (done, finished) = oneshot::channel();
        start_inline(&Handle::current(), async move {
            woken.await.unwrap();
            let _ = done.send(());
        });
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            let _ = wake.send(());
        });
        tokio::time::timeout(Duration::from_secs(5), finished)
            .await
            .expect("resumed after the wake")
            .unwrap();
    }

    #[tokio::test]
    async fn a_task_that_needs_the_runtime_context_starts_off_the_runtime() {
        let handle = Handle::current();
        let (done, finished) = oneshot::channel();
        // A plain thread has no runtime entered; the task spawns and sleeps.
        std::thread::spawn(move || {
            let runtime = handle.clone();
            start_inline(&runtime, async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                let _ = done.send(());
            });
        });
        tokio::time::timeout(Duration::from_secs(5), finished)
            .await
            .expect("ran")
            .unwrap();
    }

    #[tokio::test]
    async fn a_handler_that_panics_before_its_first_await_leaves_the_caller_running() {
        let log = Arc::new(Mutex::new(Vec::new()));
        start_inline(&Handle::current(), async {
            panic!("a handler failed");
        });
        // The caller got here, and the next message still starts and finishes.
        let (done, finished) = oneshot::channel();
        let after = Arc::clone(&log);
        start_inline(&Handle::current(), async move {
            record(&after, "next");
            let _ = done.send(());
        });
        finished.await.unwrap();
        assert_eq!(*log.lock().unwrap(), ["next"]);
    }

    #[tokio::test]
    async fn a_panic_after_the_first_await_ends_the_task_for_good() {
        use std::future::poll_fn;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::task::{Poll, Waker};

        let polls = Arc::new(AtomicUsize::new(0));
        let waker: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let (seen_polls, seen_waker) = (Arc::clone(&polls), Arc::clone(&waker));
        start_inline(
            &Handle::current(),
            poll_fn(move |context| {
                if seen_polls.fetch_add(1, Ordering::SeqCst) == 0 {
                    *seen_waker.lock().unwrap() = Some(context.waker().clone());
                    return Poll::Pending;
                }
                panic!("a handler failed after its first await");
            }),
        );
        let wake = || waker.lock().unwrap().clone().unwrap().wake();
        // The first wake polls on the runtime and panics there.
        wake();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(polls.load(Ordering::SeqCst), 2);
        // A later wake finds the task gone: the panicked future is not polled.
        wake();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(polls.load(Ordering::SeqCst), 2);
    }
}
