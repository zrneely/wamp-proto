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
            let new_waker = cx.waker().clone();
            let mut wakers = self.wakers.lock();
            if !wakers.iter().any(|w| w.will_wake(new_waker)) {
                wakers.push(new_waker);
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

/// A set which registers interest in a potential value if a query finds no result. Requires
/// external synchronization.
#[derive(Debug)]
pub struct PollableSet<T> {
    // The actual set of items we are aware of.
    items: Vec<T>,
    // The tasks to notify when a new value is added.
    waker: Vec<Waker>,
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
            items: Vec::new(),
            wakers: Vec::with_capacity(1),
        }
    }

    /// Queries the size of the set at this particular instant.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Checks if there are no items in the set.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Looks for a value in the set matching the given predicate. If one is found, returns it and
    /// removes it from the set. If not, registers the current task's interest in the set and
    /// will notify the task when a value is added.
    ///
    /// If multiple values match the predicate, only the first added is returned.
    pub fn poll_take<F>(&mut self, cx: &mut Context, mut predicate: F) -> Poll<T>
    where
        F: FnMut(&T) -> bool,
    {
        match self.items.iter().enumerate().find(|&(_, t)| predicate(t)) {
            Some((idx, _)) => {
                let value = self.items.remove(idx);
                Poll::Ready(value)
            }
            None => {
                let new_waker = cx.waker().clone();
                if !self.wakers.iter().any(|w| w.will_wake(new_waker)) {
                    self.wakers.push(new_waker);
                }
                Poll::Pending
            }
        }
    }

    /// Inserts a value into the set. This will trigger a notification for every task which has
    /// previously called poll_take and received [`Async::Pending`].
    pub fn insert(&mut self, value: T) {
        self.items.push(value);

        // Notify all registered tasks that a new value was added.
        for waker in self.wakers.drain(..) {
            waker.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use parking_lot::Mutex;
    use std::sync::Arc;
    use tokio::future::poll_fn;

    #[test]
    fn pollable_set_test() {
        let runtime = tokio::runtime::current_thread::Runtime::new().unwrap();

        let set = Arc::new(Mutex::new(PollableSet::<u32>::new()));
        assert_eq!(0, set.lock().items.len());
        assert_eq!(0, set.lock().len());

        set.lock().insert(6);
        assert_eq!(1, set.lock().items.len());
        assert_eq!(1, set.lock().len());

        set.lock().insert(4);
        set.lock().insert(8);
        assert_eq!(3, set.lock().items.len());
        assert_eq!(3, set.lock().len());

        set.lock().insert(10);
        {
            let odd_query = poll_fn(|| -> Result<Poll<bool>, ()> {
                match set.lock().poll_take(|x| x % 2 == 1) {
                    Poll::Ready(_) => Ok(Poll::Ready(true)),
                    Poll::Pending => Ok(Poll::Ready(false)),
                }
            });
            let even_query = poll_fn(|| -> Result<Poll<bool>, ()> {
                match set.lock().poll_take(|x| x % 2 == 0) {
                    Poll::Ready(_) => Ok(Poll::Ready(true)),
                    Poll::Pending => Ok(Poll::Ready(false)),
                }
            });

            // Wait for all the queries to complete
            assert_eq!(runtime.block_on(odd_query), Ok(false));
            assert_eq!(runtime.block_on(even_query), Ok(true));
        }

        // 3 (before the queries are created), 10 is inserted, and the query consumes one value.
        assert_eq!(3, set.lock().items.len());
        assert_eq!(3, set.lock().len());
    }
}
