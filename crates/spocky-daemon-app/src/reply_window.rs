//! Per-agent update tails and the reply window of a `wait_for_finish`.
//!
//! Pinned `session.ts` gets "the wait's reply before the update its state
//! change caused" from await depth alone: the manager dispatches
//! `agent_state` synchronously, the session's forwarder only enqueues the
//! update on that agent's own tail (`liveAgentUpdateTails`), and the update
//! then goes through enrichment and placement awaits before it emits, while
//! the woken wait emits within microtasks. Tokio carries no such ordering,
//! so the dispatch itself decides: when a state change dispatches that wakes
//! a wait, the update that dispatch enqueues is held, on its own agent's
//! tail only, until that wait has replied.
//!
//! Nothing else is held: an update that wakes no wait, any other agent's
//! tail, and `agent.create`'s forward all go out at once.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{Notify, mpsc, oneshot};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a dispatched `agent_state` event says about its agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispatched {
    pub agent_id: String,
    /// The agent is running, or has a foreground turn.
    pub active: bool,
    /// The state settles a `wait_for_finish`: idle or error with no turn
    /// running, or a permission pending.
    pub settles: bool,
}

/// One wait's reply, as a window an update can be held behind.
#[derive(Default)]
pub struct Window {
    released: AtomicBool,
    notify: Notify,
}

impl Window {
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Returns once the reply is out, or after `bound`.
    async fn released(&self, bound: Duration) {
        let released = async {
            loop {
                let notified = self.notify.notified();
                if self.released.load(Ordering::SeqCst) {
                    return;
                }
                notified.await;
            }
        };
        let _ = tokio::time::timeout(bound, released).await;
    }
}

struct WaitState {
    agent_id: String,
    window: Arc<Window>,
    /// The agent has been active since the wait began: only then can a
    /// settling state wake a `waitForActive` wait.
    active: AtomicBool,
    /// This wait's wake has already been taken by one dispatch.
    woken: AtomicBool,
}

/// The `wait_for_finish` requests in flight.
#[derive(Default)]
pub struct Waits {
    entries: Mutex<Vec<Arc<WaitState>>>,
}

/// A `wait_for_finish` in flight; dropping it is its reply being out.
pub struct WaitGuard {
    waits: Arc<Waits>,
    state: Arc<WaitState>,
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        lock(&self.waits.entries).retain(|state| !Arc::ptr_eq(state, &self.state));
        self.state.window.release();
    }
}

impl Waits {
    /// Registers a wait on `agent_id`; `active` is whether the agent is
    /// running now.
    #[must_use]
    pub fn begin(self: &Arc<Self>, agent_id: &str, active: bool) -> WaitGuard {
        let state = Arc::new(WaitState {
            agent_id: agent_id.to_owned(),
            window: Arc::new(Window::default()),
            active: AtomicBool::new(active),
            woken: AtomicBool::new(false),
        });
        lock(&self.entries).push(Arc::clone(&state));
        WaitGuard {
            waits: Arc::clone(self),
            state,
        }
    }

    /// Called inside the dispatch of a state change: the windows of the
    /// waits this change wakes, which the update it enqueues is held behind.
    /// A wait is woken once, by the first settling change after it saw the
    /// agent active.
    #[must_use]
    pub fn dispatch(&self, event: &Dispatched) -> Vec<Arc<Window>> {
        let mut held = Vec::new();
        for state in lock(&self.entries).iter() {
            if state.agent_id != event.agent_id {
                continue;
            }
            let was_active = state.active.load(Ordering::SeqCst);
            if event.active {
                state.active.store(true, Ordering::SeqCst);
            }
            if event.settles
                && (was_active || event.active)
                && !state.woken.swap(true, Ordering::SeqCst)
            {
                held.push(Arc::clone(&state.window));
            }
        }
        held
    }
}

/// An update to publish, and what it waits behind.
pub struct Job<T> {
    /// `None` for a flush marker.
    pub item: Option<T>,
    pub holds: Vec<Arc<Window>>,
    pub done: Option<oneshot::Sender<()>>,
}

type Publish<T> = Arc<dyn Fn(T) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// `liveAgentUpdateTails`: one ordered queue per agent, each drained by its
/// own task, so a held update stalls only its own agent.
pub struct Tails<T> {
    runtime: tokio::runtime::Handle,
    publish: Publish<T>,
    /// How long a held update waits for a reply that does not come.
    bound: Duration,
    queues: Mutex<HashMap<String, mpsc::UnboundedSender<Job<T>>>>,
}

impl<T: Send + 'static> Tails<T> {
    #[must_use]
    pub fn new(runtime: tokio::runtime::Handle, bound: Duration, publish: Publish<T>) -> Self {
        Self {
            runtime,
            publish,
            bound,
            queues: Mutex::new(HashMap::new()),
        }
    }

    /// Queues `job` on the tail of `agent_id`, starting the tail if needed.
    pub fn submit(&self, agent_id: &str, job: Job<T>) {
        let mut queues = lock(&self.queues);
        let sender = queues.entry(agent_id.to_owned()).or_insert_with(|| {
            let (sender, mut jobs) = mpsc::unbounded_channel::<Job<T>>();
            let publish = Arc::clone(&self.publish);
            let bound = self.bound;
            self.runtime.spawn(async move {
                while let Some(job) = jobs.recv().await {
                    for window in &job.holds {
                        window.released(bound).await;
                    }
                    if let Some(item) = job.item {
                        publish(item).await;
                    }
                    if let Some(done) = job.done {
                        let _ = done.send(());
                    }
                }
            });
            sender
        });
        let _ = sender.send(job);
    }

    /// Returns once everything queued on `agent_id`'s tail so far is out.
    pub async fn flush(&self, agent_id: &str) {
        let (done, flushed) = oneshot::channel();
        self.submit(
            agent_id,
            Job {
                item: None,
                holds: Vec::new(),
                done: Some(done),
            },
        );
        let _ = flushed.await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{Dispatched, Job, Tails, Waits};

    fn event(agent: &str, active: bool, settles: bool) -> Dispatched {
        Dispatched {
            agent_id: agent.to_owned(),
            active,
            settles,
        }
    }

    fn tails(published: &Arc<Mutex<Vec<String>>>, bound: Duration) -> Tails<String> {
        let published = Arc::clone(published);
        Tails::new(
            tokio::runtime::Handle::current(),
            bound,
            Arc::new(move |item: String| {
                let published = Arc::clone(&published);
                Box::pin(async move { published.lock().unwrap().push(item) })
            }),
        )
    }

    fn published(published: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        published.lock().unwrap().clone()
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(60)).await;
    }

    #[test]
    fn a_wait_is_woken_once_by_the_first_settling_change_after_it_saw_activity() {
        let waits = Arc::new(Waits::default());
        let _wait = waits.begin("a", false);
        assert!(waits.dispatch(&event("a", false, true)).is_empty());
        assert!(waits.dispatch(&event("a", true, false)).is_empty());
        assert_eq!(waits.dispatch(&event("a", false, true)).len(), 1);
        assert!(waits.dispatch(&event("a", false, true)).is_empty());
    }

    #[test]
    fn waits_on_other_agents_are_not_woken() {
        let waits = Arc::new(Waits::default());
        let _wait = waits.begin("a", true);
        assert!(waits.dispatch(&event("b", false, true)).is_empty());
    }

    #[test]
    fn a_wait_that_started_on_a_running_agent_wakes_on_the_first_settle() {
        let waits = Arc::new(Waits::default());
        let _wait = waits.begin("a", true);
        assert_eq!(waits.dispatch(&event("a", false, true)).len(), 1);
    }

    #[tokio::test]
    async fn a_non_waking_idle_update_is_not_delayed() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let tails = tails(&out, Duration::from_secs(30));
        let waits = Arc::new(Waits::default());
        // A `waitForActive` wait on an idle agent: an idle update does not wake it.
        let _wait = waits.begin("a", false);
        let holds = waits.dispatch(&event("a", false, true));
        assert!(holds.is_empty());
        tails.submit(
            "a",
            Job {
                item: Some("idle".to_owned()),
                holds,
                done: None,
            },
        );
        settle().await;
        assert_eq!(published(&out), ["idle"]);
    }

    #[tokio::test]
    async fn another_agent_is_not_stalled_while_one_is_held() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let tails = tails(&out, Duration::from_secs(30));
        let waits = Arc::new(Waits::default());
        let wait_a = waits.begin("a", true);
        let holds = waits.dispatch(&event("a", false, true));
        assert_eq!(holds.len(), 1);
        tails.submit(
            "a",
            Job {
                item: Some("a-settled".to_owned()),
                holds,
                done: None,
            },
        );
        tails.submit(
            "b",
            Job {
                item: Some("b-update".to_owned()),
                holds: Vec::new(),
                done: None,
            },
        );
        settle().await;
        assert_eq!(published(&out), ["b-update"], "b went out; a is held");
        drop(wait_a);
        settle().await;
        assert_eq!(published(&out), ["b-update", "a-settled"]);
    }

    #[tokio::test]
    async fn later_updates_of_a_held_agent_stay_in_order_behind_it() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let tails = tails(&out, Duration::from_secs(30));
        let waits = Arc::new(Waits::default());
        let wait = waits.begin("a", true);
        let holds = waits.dispatch(&event("a", false, true));
        for (item, holds) in [("first", holds), ("second", Vec::new())] {
            tails.submit(
                "a",
                Job {
                    item: Some(item.to_owned()),
                    holds,
                    done: None,
                },
            );
        }
        settle().await;
        assert!(published(&out).is_empty());
        drop(wait);
        settle().await;
        assert_eq!(published(&out), ["first", "second"]);
    }

    #[tokio::test]
    async fn the_hold_is_bounded() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let tails = tails(&out, Duration::from_millis(40));
        let waits = Arc::new(Waits::default());
        let _wait = waits.begin("a", true);
        let holds = waits.dispatch(&event("a", false, true));
        tails.submit(
            "a",
            Job {
                item: Some("late".to_owned()),
                holds,
                done: None,
            },
        );
        settle().await;
        assert_eq!(
            published(&out),
            ["late"],
            "published with the wait still open"
        );
    }

    #[tokio::test]
    async fn flush_waits_only_for_its_own_agent() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let tails = tails(&out, Duration::from_secs(30));
        let waits = Arc::new(Waits::default());
        let wait_a = waits.begin("a", true);
        let holds = waits.dispatch(&event("a", false, true));
        tails.submit(
            "a",
            Job {
                item: Some("a".to_owned()),
                holds,
                done: None,
            },
        );
        tokio::time::timeout(Duration::from_secs(5), tails.flush("b"))
            .await
            .expect("b's flush is not behind a's hold");
        drop(wait_a);
        tails.flush("a").await;
        assert_eq!(published(&out), ["a"]);
    }
}
