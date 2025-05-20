// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements a combined mutex and condition.
//!
//! # Example:
//!
//! ```no_run
//!     let condition = Condition::new(0);
//!     condition.when(|state| if state == 1 { Poll::Ready(()) } else { Poll::Pending }).await;
//!
//!     // Elsewhere...
//!     let guard = condition.lock();
//!     *guard.lock() = 1;
//!     for waker in guard.drain_wakers() {
//!         waker.wake();
//!     }
//! ```

use std::future::poll_fn;
use std::marker::PhantomPinned;
use std::ops::{Deref, DerefMut};
use std::pin::{pin, Pin};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Poll, Waker};

/// An async condition which combines a mutex and a condition variable.
// Condition is implemented as an intrusive doubly linked list.  Typical use should avoid any
// additional heap allocations after creation, as the nodes of the list are stored as part of the
// caller's future.
pub struct Condition<T>(Arc<Mutex<Inner<T>>>);

impl<T> Condition<T> {
    /// Returns a new condition.
    pub fn new(data: T) -> Self {
        Self(Arc::new(Mutex::new(Inner { head: None, count: 0, data })))
    }

    /// Returns the number of wakers waiting on the condition.
    pub fn waker_count(&self) -> usize {
        self.0.lock().unwrap().count
    }

    /// Same as `Mutex::lock`.
    pub fn lock(&self) -> ConditionGuard<'_, T> {
        ConditionGuard(&self.0, self.0.lock().unwrap())
    }

    /// Returns when `poll` resolves.
    pub async fn when<R>(&self, poll: impl Fn(&mut T) -> Poll<R>) -> R {
        let mut entry = WakerEntry::new();
        entry.list = Some(self.0.clone());
        let mut entry = pin!(entry);
        poll_fn(|cx| {
            let mut guard = self.0.lock().unwrap();
            // SAFETY: We uphold the pin guarantee.
            let entry = unsafe { entry.as_mut().get_unchecked_mut() };
            let result = poll(&mut guard.data);
            if result.is_pending() {
                // SAFETY: We set list correctly above.
                unsafe {
                    entry.node.add(&mut *guard, cx.waker().clone());
                }
            }
            result
        })
        .await
    }
}

struct Inner<T> {
    head: Option<NonNull<Node>>,
    count: usize,
    data: T,
}

// SAFETY: Safe because we always access `head` whilst holding the list lock.
unsafe impl<T: Send> Send for Inner<T> {}

/// Guard returned by `lock`.
pub struct ConditionGuard<'a, T>(&'a Arc<Mutex<Inner<T>>>, MutexGuard<'a, Inner<T>>);

impl<'a, T> ConditionGuard<'a, T> {
    /// Adds the waker entry to the condition's list of wakers.
    pub fn add_waker(&mut self, waker_entry: Pin<&mut WakerEntry<T>>, waker: Waker) {
        // SAFETY: We never move the data out.
        let waker_entry = unsafe { waker_entry.get_unchecked_mut() };
        waker_entry.list = Some(self.0.clone());
        // SAFETY: We set list correctly above.
        unsafe {
            waker_entry.node.add(&mut *self.1, waker);
        }
    }

    /// Returns an iterator that will drain all wakers.  Whilst the drainer exists, a lock is held
    /// which will prevent new wakers from being added to the list, so depending on your use case,
    /// you might wish to collect the wakers before calling `wake` on each waker.  NOTE: If the
    /// drainer is dropped, this will *not* drain elements not visited.
    pub fn drain_wakers<'b>(&'b mut self) -> Drainer<'b, 'a, T> {
        Drainer(self)
    }

    /// Returns the number of wakers registered with the condition.
    pub fn waker_count(&self) -> usize {
        self.1.count
    }
}

impl<T> Deref for ConditionGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.1.data
    }
}

impl<T> DerefMut for ConditionGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.1.data
    }
}

/// A waker entry that can be added to a list.
pub struct WakerEntry<T> {
    list: Option<Arc<Mutex<Inner<T>>>>,
    node: Node,
}

impl<T> WakerEntry<T> {
    /// Returns a new entry.
    pub fn new() -> Self {
        Self {
            list: None,
            node: Node { next: None, prev: None, waker: None, _pinned: PhantomPinned },
        }
    }
}

impl<T> Default for WakerEntry<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Drop for WakerEntry<T> {
    fn drop(&mut self) {
        if let Some(list) = &self.list {
            self.node.remove(&mut *list.lock().unwrap());
        }
    }
}

// The members here must only be accessed whilst holding the mutex on the list.
struct Node {
    next: Option<NonNull<Node>>,
    prev: Option<NonNull<Node>>,
    waker: Option<Waker>,
    _pinned: PhantomPinned,
}

// SAFETY: Safe because we always access all mebers of `Node` whilst holding the list lock.
unsafe impl Send for Node {}

impl Node {
    // # Safety
    //
    // The waker *must* have `list` set correctly.
    unsafe fn add<T>(&mut self, inner: &mut Inner<T>, waker: Waker) {
        if self.waker.is_none() {
            self.prev = None;
            self.next = inner.head;
            inner.head = Some(self.into());
            if let Some(mut next) = self.next {
                // SAFETY: Safe because we have exclusive access to `Inner` and `head` is set
                // correctly above.
                unsafe {
                    next.as_mut().prev = Some(self.into());
                }
            }
            inner.count += 1;
        }
        self.waker = Some(waker);
    }

    fn remove<T>(&mut self, inner: &mut Inner<T>) -> Option<Waker> {
        if self.waker.is_none() {
            debug_assert!(self.prev.is_none() && self.next.is_none());
            return None;
        }
        if let Some(mut next) = self.next {
            // SAFETY: Safe because we have exclusive access to `Inner` and `head` is set correctly.
            unsafe { next.as_mut().prev = self.prev };
        }
        if let Some(mut prev) = self.prev {
            // SAFETY: Safe because we have exclusive access to `Inner` and `head` is set correctly.
            unsafe { prev.as_mut().next = self.next };
        } else {
            inner.head = self.next;
        }
        self.prev = None;
        self.next = None;
        inner.count -= 1;
        self.waker.take()
    }
}

/// An iterator that will drain waiters.
pub struct Drainer<'a, 'b, T>(&'a mut ConditionGuard<'b, T>);

impl<T> Iterator for Drainer<'_, '_, T> {
    type Item = Waker;
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(mut head) = self.0 .1.head {
            // SAFETY: Safe because we have exclusive access to `Inner` and `head is set correctly.
            unsafe { head.as_mut().remove(&mut self.0 .1) }
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.0 .1.count, Some(self.0 .1.count))
    }
}

impl<T> ExactSizeIterator for Drainer<'_, '_, T> {
    fn len(&self) -> usize {
        self.0 .1.count
    }
}

#[cfg(all(target_os = "fuchsia", test))]
mod tests {
    use super::{Condition, WakerEntry};
    use crate::TestExecutor;
    use futures::stream::FuturesUnordered;
    use futures::task::noop_waker;
    use futures::StreamExt;
    use std::pin::pin;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::task::Poll;

    #[test]
    fn test_condition_can_waker_multiple_wakers() {
        let mut executor = TestExecutor::new();
        let condition = Condition::new(());

        static COUNT: u64 = 10;

        let counter = AtomicU64::new(0);

        // Use FuturesUnordered so that futures are only polled when explicitly woken.
        let mut futures = FuturesUnordered::new();

        for _ in 0..COUNT {
            futures.push(condition.when(|()| {
                if counter.fetch_add(1, Ordering::Relaxed) >= COUNT {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }));
        }

        assert!(executor.run_until_stalled(&mut futures.next()).is_pending());

        assert_eq!(counter.load(Ordering::Relaxed), COUNT);
        assert_eq!(condition.waker_count(), COUNT as usize);

        {
            let mut guard = condition.lock();
            let drainer = guard.drain_wakers();
            assert_eq!(drainer.len(), COUNT as usize);
            for waker in drainer {
                waker.wake();
            }
        }

        assert!(executor.run_until_stalled(&mut futures.collect::<Vec<_>>()).is_ready());
        assert_eq!(counter.load(Ordering::Relaxed), COUNT * 2);
    }

    #[test]
    fn test_dropping_waker_entry_removes_from_list() {
        let condition = Condition::new(());

        let entry1 = pin!(WakerEntry::new());
        condition.lock().add_waker(entry1, noop_waker());

        {
            let entry2 = pin!(WakerEntry::new());
            condition.lock().add_waker(entry2, noop_waker());

            assert_eq!(condition.waker_count(), 2);
        }

        assert_eq!(condition.waker_count(), 1);
        {
            let mut guard = condition.lock();
            assert_eq!(guard.drain_wakers().count(), 1);
        }

        assert_eq!(condition.waker_count(), 0);

        let entry3 = pin!(WakerEntry::new());
        condition.lock().add_waker(entry3, noop_waker());

        assert_eq!(condition.waker_count(), 1);
    }

    #[test]
    fn test_waker_can_be_added_multiple_times() {
        let condition = Condition::new(());

        let mut entry1 = pin!(WakerEntry::new());
        condition.lock().add_waker(entry1.as_mut(), noop_waker());

        let mut entry2 = pin!(WakerEntry::new());
        condition.lock().add_waker(entry2.as_mut(), noop_waker());

        assert_eq!(condition.waker_count(), 2);
        {
            let mut guard = condition.lock();
            assert_eq!(guard.drain_wakers().count(), 2);
        }
        assert_eq!(condition.waker_count(), 0);

        condition.lock().add_waker(entry1, noop_waker());
        condition.lock().add_waker(entry2, noop_waker());

        assert_eq!(condition.waker_count(), 2);

        {
            let mut guard = condition.lock();
            assert_eq!(guard.drain_wakers().count(), 2);
        }
        assert_eq!(condition.waker_count(), 0);
    }
}
