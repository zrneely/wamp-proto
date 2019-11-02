use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use parking_lot::{Mutex, RwLock};

/// A variable, shared between threads/tasks. When you read from this variable, you also
/// implicitly register interest in future values of the variable as well, and the current
/// task will be notified when the value changes.
///
/// From a usage perspective, this is basically an Arc<Mutex<T>> which will notify any task
/// that reads from it when the value is updated in the future.
#[derive(Clone, Debug)]
pub struct PollableValue<T: Copy> {
    val: Arc<RwLock<T>>,
    wakers: Arc<Mutex<Vec<Waker>>>,
}
impl<T> PollableValue<T>
where
    T: Copy,
{
    /// Creates a new pollable value.
    pub fn new(value: T) -> Self {
        PollableValue {
            val: Arc::new(RwLock::new(value)),
            wakers: Arc::new(Mutex::new(Vec::with_capacity(1))),
        }
    }

    /// Copies the value. If task context is provided, registers the task's interest in future
    /// values of the variable.
    pub fn read(&self, cx: Option<&mut Context>) -> T {
        if let Some(ref mut cx) = cx {
            // We clone the waker instead of storing a ref and using wake_by_ref because
            // the actual waking operation happens in another method, and we have no way
            // to tie the lifetime of the current context to the time that method is called.
            let new_waker = cx.waker();
            let mut wakers = self.wakers.lock();
            if !wakers.iter().any(|w| w.will_wake(new_waker)) {
                wakers.push(new_waker.clone());
            }
        }

        *self.val.read()
    }

    /// Assigns a new value to this variable. This will notify all tasks which have previously
    /// called [`read`] on this value.
    pub fn write(&self, val: T) {
        *self.val.write() = val;

        // Notify all registered tasks that a new value was added.
        let mut wakers = self.wakers.lock();
        for waker in wakers.drain(..) {
            waker.wake();
        }
    }
}

/// A set which registers interest in a potential value if a query finds no result.
/// Can be cheaply cloned (clones refer to the same internal structures using Arc).
/// Does synchronization internally.
#[derive(Clone, Debug)]
pub struct PollableSet<T> {
    // The actual set of items we are aware of.
    items: Arc<RwLock<Vec<T>>>,
    // The tasks to notify when a new value is added.
    wakers: Arc<Mutex<Vec<Waker>>>,
}
impl<T> Default for PollableSet<T> {
    fn default() -> Self {
        PollableSet::new()
    }
}
impl<T> PollableSet<T> {
    /// Creates a new pollable set.
    pub fn new() -> Self {
        PollableSet {
            items: Arc::new(RwLock::new(Vec::new())),
            wakers: Arc::new(Mutex::new(Vec::with_capacity(1))),
        }
    }

    /// Queries the size of the set at this particular instant.
    pub fn len(&self) -> usize {
        self.items.read().len()
    }

    /// Checks if there are no items in the set.
    pub fn is_empty(&self) -> bool {
        self.items.read().is_empty()
    }

    /// Looks for a value in the set matching the given predicate. If one is found, returns it and
    /// removes it from the set. If not, registers the current task's interest in the set and
    /// will notify the task when a value is added.
    ///
    /// If multiple values match the predicate, only the first added is returned.
    pub fn poll_take<F>(&self, cx: &mut Context, mut predicate: F) -> Poll<T>
    where
        F: FnMut(&T) -> bool,
    {
        let items = self.items.write();
        match items.iter().enumerate().find(|&(_, t)| predicate(t)) {
            Some((idx, _)) => {
                let value = items.remove(idx);
                Poll::Ready(value)
            }
            None => {
                let new_waker = cx.waker();
                let wakers = self.wakers.lock();
                if !wakers.iter().any(|w| w.will_wake(new_waker)) {
                    wakers.push(new_waker.clone());
                }
                Poll::Pending
            }
        }
    }

    /// See `poll_take` (but this will assume the predicate always passes).
    pub fn poll_take_any(&self, cx: &mut Context) -> Poll<T> {
        self.poll_take(cx, |_| true)
    }

    /// Synchronously removes all items matching the given predicate.
    pub fn drain<F: FnMut(&T) -> bool>(&self, mut predicate: F) {
        let mut items = self.items.write();
        let mut i = 0;
        while i != items.len() {
            if predicate(&items[i]) {
                items.remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Synchronously removes all items from the queue.
    pub fn clear(&self) {
        self.items.write().clear()
    }

    /// Inserts a value into the set. This will trigger a notification for every task which has
    /// previously called poll_take and received [`Poll::Pending`].
    pub fn insert(&self, value: T) {
        self.items.write().push(value);

        // Notify all registered tasks that a new value was added.
        for waker in self.wakers.lock().drain(..) {
            waker.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::future::poll_fn;

    #[test]
    fn pollable_set_test() {
        let runtime = tokio::runtime::current_thread::Runtime::new().unwrap();

        let set = PollableSet::<u32>::new();
        assert_eq!(0, set.items.read().len());
        assert_eq!(0, set.len());

        set.insert(6);
        assert_eq!(1, set.items.read().len());
        assert_eq!(1, set.len());

        set.insert(4);
        set.insert(8);
        assert_eq!(3, set.items.read().len());
        assert_eq!(3, set.len());

        set.insert(10);
        {
            let odd_query = poll_fn(|cx| -> Poll<bool> {
                match set.poll_take(cx, |x| x % 2 == 1) {
                    Poll::Ready(_) => Poll::Ready(true),
                    Poll::Pending => Poll::Ready(false),
                }
            });
            let even_query = poll_fn(|cx| -> Poll<bool> {
                match set.poll_take(cx, |x| x % 2 == 0) {
                    Poll::Ready(_) => Poll::Ready(true),
                    Poll::Pending => Poll::Ready(false),
                }
            });

            // Wait for all the queries to complete
            assert_eq!(runtime.block_on(odd_query), false);
            assert_eq!(runtime.block_on(even_query), true);
        }

        // 3 (before the queries are created), 10 is inserted, and the query consumes one value.
        assert_eq!(3, set.items.read().len());
        assert_eq!(3, set.len());
    }
}
