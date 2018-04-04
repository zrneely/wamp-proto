
use futures::Async;
use futures::task::{Context, Waker};

/// A set which registers interest in a potential value if a query finds no result. Requires
/// external synchronization.
#[derive(Debug)]
pub struct PollableSet<T> {
    // The actual set of items we are aware of.
    items: Vec<T>,
    // The tasks to notify when a new value is added.
    tasks: Vec<Waker>,
}
impl<T> Default for PollableSet<T> {
    fn default() -> Self {
        PollableSet {
            items: Vec::new(),
            tasks: Vec::new(),
        }
    }
}
impl <T> PollableSet<T> {

    /// Creates a new pollable set.
    pub fn new() -> Self {
        PollableSet {
            items: Vec::new(),
            tasks: Vec::with_capacity(1),
        }
    }

    /// Looks for a value in the set matching the given predicate. If one is found, returns it and
    /// removes it from the set. If not, registers the current [`task`]'s interest in the set and
    /// will notify the task when a value is added.
    ///
    /// If multiple values match the predicate, only the first added is returned.
    pub fn poll_take<F>(&mut self, mut predicate: F, cx: &mut Context) -> Async<T>
        where F: FnMut(&T) -> bool {
        match self.items.iter().enumerate().filter(|&(_, t): &(usize, &T)| predicate(t)).next() {
            Some((idx, _)) => {
                let value = self.items.remove(idx);
                Async::Ready(value)
            }
            None => {
                let waker = cx.waker();
                if !self.tasks.iter().any(|w| waker.will_wake(w)) {
                    // The current task is not yet interested, so add it to the list of interested
                    // tasks.
                    self.tasks.push(waker.clone());
                }
                Async::Pending
            }
        }
    }

    /// Inserts a value into the set. This will trigger a notification for every task which has
    /// previously called poll_take and received [`Async::Pending`].
    pub fn insert(&mut self, value: T) {
        self.items.push(value);

        // Notify all registered tasks that a new value was added.
        for task in self.tasks.drain(..) {
            task.wake();
        }
    }
}