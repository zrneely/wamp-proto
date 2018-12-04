use std::sync::Arc;

use futures::task::{self, Task};
use futures::Async;
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
    tasks: Arc<Mutex<Vec<Task>>>,
}
impl<T> PollableValue<T>
where
    T: Copy,
{
    /// Creates a new pollable value.
    pub fn new(value: T) -> Self {
        PollableValue {
            val: Arc::new(RwLock::new(value)),
            tasks: Arc::new(Mutex::new(Vec::with_capacity(1))),
        }
    }

    /// Takes a read lock on the value. If notify is true, registers the current task's interest in future
    /// values of the variable.
    pub fn read(&self, notify: bool) -> T {
        if notify {
            let task = task::current();
            let mut lock = self.tasks.lock();
            if !lock.iter().any(|t| t.will_notify_current()) {
                // The current task is not yet interested, so add it to the list of interested
                // tasks.
                lock.push(task);
            }
        }

        *self.val.read()
    }

    /// Assigns a new value to this variable. This will notify all tasks which have previously
    /// called [`read`] on this value.
    pub fn write(&self, val: T) {
        *self.val.write() = val;

        // Notify all registered tasks that a new value was added.
        let mut lock = self.tasks.lock();
        for task in lock.drain(..) {
            task.notify();
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
    tasks: Vec<Task>,
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
            tasks: Vec::with_capacity(1),
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
    /// removes it from the set. If not, registers the current [`task`]'s interest in the set and
    /// will notify the task when a value is added.
    ///
    /// If multiple values match the predicate, only the first added is returned.
    pub fn poll_take<F>(&mut self, mut predicate: F) -> Async<T>
    where
        F: FnMut(&T) -> bool,
    {
        match self.items.iter().enumerate().find(|&(_, t)| predicate(t)) {
            Some((idx, _)) => {
                let value = self.items.remove(idx);
                Async::Ready(value)
            }
            None => {
                let task = task::current();
                if !self.tasks.iter().any(|t| t.will_notify_current()) {
                    // The current task is not yet interested, so add it to the list of interested
                    // tasks.
                    self.tasks.push(task);
                }
                Async::NotReady
            }
        }
    }

    /// Inserts a value into the set. This will trigger a notification for every task which has
    /// previously called poll_take and received [`Async::Pending`].
    pub fn insert(&mut self, value: T) {
        self.items.push(value);

        // Notify all registered tasks that a new value was added.
        for task in self.tasks.drain(..) {
            task.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::future::poll_fn;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use tokio::runtime::current_thread;

    #[test]
    fn pollable_set_test() {
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
            let odd_query = poll_fn(|| -> Result<Async<bool>, ()> {
                match set.lock().poll_take(|x| x % 2 == 1) {
                    Async::Ready(_) => Ok(Async::Ready(true)),
                    Async::NotReady => Ok(Async::Ready(false)),
                }
            });
            let even_query = poll_fn(|| -> Result<Async<bool>, ()> {
                match set.lock().poll_take(|x| x % 2 == 0) {
                    Async::Ready(_) => Ok(Async::Ready(true)),
                    Async::NotReady => Ok(Async::Ready(false)),
                }
            });

            // Wait for all the queries to complete
            assert_eq!(current_thread::block_on_all(odd_query), Ok(false));
            assert_eq!(current_thread::block_on_all(even_query), Ok(true));
        }

        // 3 (before the queries are created), 10 is inserted, and the query consumes one value.
        assert_eq!(3, set.lock().items.len());
        assert_eq!(3, set.lock().len());
    }
}
