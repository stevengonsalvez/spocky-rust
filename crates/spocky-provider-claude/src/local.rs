//! Single-threaded promise and stream primitives. The session and the SDK
//! port run on one thread with a `tokio::task::LocalSet`, as the baseline
//! runs on one Node event loop, so state is `Rc<RefCell<..>>` and code runs
//! uninterrupted between awaits.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use tokio::sync::Notify;

/// A boxed future that stays on the session thread.
pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// A settle-once value many tasks may await, like a shared `Promise`.
pub struct Deferred<T: Clone> {
    value: RefCell<Option<T>>,
    notify: Notify,
}

impl<T: Clone> Default for Deferred<T> {
    fn default() -> Self {
        Self {
            value: RefCell::new(None),
            notify: Notify::new(),
        }
    }
}

impl<T: Clone> Deferred<T> {
    /// A new unsettled value.
    #[must_use]
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// Settles the value; later calls are ignored.
    pub fn settle(&self, value: T) {
        let mut slot = self.value.borrow_mut();
        if slot.is_none() {
            *slot = Some(value);
            drop(slot);
            self.notify.notify_waiters();
        }
    }

    /// The value, if settled.
    #[must_use]
    pub fn peek(&self) -> Option<T> {
        self.value.borrow().clone()
    }

    /// Waits for the value.
    pub async fn wait(&self) -> T {
        loop {
            let notified = self.notify.notified();
            if let Some(value) = self.peek() {
                return value;
            }
            notified.await;
        }
    }
}

enum QueueEnd<E> {
    Open,
    Done,
    Failed(E),
}

/// An async queue that yields items, then ends or fails, like the SDK's
/// message stream (`Lf`).
pub struct AsyncQueue<T, E: Clone> {
    items: RefCell<VecDeque<T>>,
    end: RefCell<QueueEnd<E>>,
    notify: Notify,
}

impl<T, E: Clone> Default for AsyncQueue<T, E> {
    fn default() -> Self {
        Self {
            items: RefCell::new(VecDeque::new()),
            end: RefCell::new(QueueEnd::Open),
            notify: Notify::new(),
        }
    }
}

impl<T, E: Clone> AsyncQueue<T, E> {
    /// `enqueue(item)`.
    pub fn enqueue(&self, item: T) {
        self.items.borrow_mut().push_back(item);
        self.notify.notify_waiters();
    }

    /// `done()`.
    pub fn done(&self) {
        let mut end = self.end.borrow_mut();
        if matches!(*end, QueueEnd::Open) {
            *end = QueueEnd::Done;
        }
        drop(end);
        self.notify.notify_waiters();
    }

    /// `error(error)`: queued items still come first.
    pub fn error(&self, error: E) {
        let mut end = self.end.borrow_mut();
        if matches!(*end, QueueEnd::Open) {
            *end = QueueEnd::Failed(error);
        }
        drop(end);
        self.notify.notify_waiters();
    }

    /// Whether the queue has ended or failed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        !matches!(*self.end.borrow(), QueueEnd::Open)
    }

    /// `next()`: an item, `None` at the end, or the failure.
    pub async fn next(&self) -> Option<Result<T, E>> {
        loop {
            let notified = self.notify.notified();
            if let Some(item) = self.items.borrow_mut().pop_front() {
                return Some(Ok(item));
            }
            match &*self.end.borrow() {
                QueueEnd::Done => return None,
                QueueEnd::Failed(error) => return Some(Err(error.clone())),
                QueueEnd::Open => {}
            }
            notified.await;
        }
    }
}

/// `new Promise((resolve) => setTimeout(resolve, 0))`: lets other ready
/// tasks on the session thread run first.
pub async fn macrotask() {
    tokio::task::yield_now().await;
}
