
use std::hash::{Hash, Hasher};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::Async;
use futures::task::{self, Task};

// /// Allows arbitrary types to be stored in [`HashMap`]s, assuming no two values will ever be
// /// equal.
// pub struct UniquelyHashable<T> {
//     id: i64,
//     value: T,
// }
// impl<T> PartialEq for UniquelyHashable<T> {
//     fn eq(&self, _other: &Self) -> bool { false }
// }
// impl<T> Eq for UniquelyHashable<T> {}
// impl<T> Hash for UniquelyHashable<T> {
//     fn hash<H: Hasher>(&self, state: &mut H) {
//         self.id.hash(state);
//     }
// }
// impl<T> Deref for UniquelyHashable<T> {
//     type Target = T;
//     fn deref(&self) -> &Self::Target {
//         &self.value
//     }
// }
// impl<T> DerefMut for UniquelyHashable<T> {
//     fn deref_mut(&mut self) -> &mut Self::Target {
//         &mut self.value
//     }
// }
// impl<T> UniquelyHashable<T> {
//     pub fn new(value: T) -> Self {
//         lazy_static! {
//             static ref NEXT2: AtomicUsize = AtomicUsize::new(1);
//         }
//         UniquelyHashable {
//             id: NEXT2.fetch_add(1, Ordering::SeqCst) as i64,
//             value
//         }
//     }
// }

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

    /// Queries the size of the set at this particular instant.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Looks for a value in the set matching the given predicate. If one is found, returns it and
    /// removes it from the set. If not, registers the current [`task`]'s interest in the set and
    /// will notify the task when a value is added.
    ///
    /// If multiple values match the predicate, only the first added is returned.
    pub fn poll_take<F>(&mut self, mut predicate: F) -> Async<T>
        where F: FnMut(&T) -> bool {
        match self.items.iter().enumerate().filter(|&(_, t): &(usize, &T)| predicate(t)).next() {
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