// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements a waker list.
//!
//! # Example:
//!
//! ```no_run
//! fn foo() {
//!     let waker_list = WakerList::new();
//!
//!     let waker_entry = pin!(waker_list.new_entry());
//!     poll_fn(|cx| {
//!         if (ready) {
//!             Poll::Ready(())
//!         } else {
//!             waker_entry.add(cx.waker().clone());
//!             Poll::Pending
//!         }
//!     }).await;
//!
//!     // Elsewhere...
//!     for waker in waker_list.drain() {
//!         waker.wake();
//!     }
//! }
//! ```

use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::Waker;

/// A waker list.
// WakerList is implemented as an intrusive doubly linked list.  Typical use should avoid any
// additional heap allocations after creation, as the nodes of the list are stored as part of the
// caller's future.
pub struct WakerList(Arc<Mutex<Inner>>);

impl WakerList {
    /// Returns a new waker list.
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Inner { head: std::ptr::null_mut(), count: 0 })))
    }

    /// Returns a new entry that can be later added to this list.
    pub fn new_entry(&self) -> WakerEntry {
        WakerEntry {
            list: self.0.clone(),
            node: Node { next: std::ptr::null_mut(), prev: std::ptr::null_mut(), waker: None },
        }
    }

    /// Returns the number of wakers in the list.
    pub fn len(&self) -> usize {
        self.0.lock().unwrap().count
    }

    /// Returns an iterator that will drain all wakers.  Whilst the drainer exists, a lock is held
    /// which will prevent new wakers from being added to the list, so depending on your use case,
    /// you might wish to collect the wakers before calling `wake` on each waker.  NOTE: If the
    /// drainer is dropped, this will *not* drain elements not visited.
    pub fn drain(&self) -> Drainer<'_> {
        Drainer(self.0.lock().unwrap())
    }
}

struct Inner {
    head: *mut Node,
    count: usize,
}

// SAFETY: Safe because we always access `head` whilst holding the list lock.
unsafe impl Send for Inner {}

/// A waker entry that can be added to a list.
pub struct WakerEntry {
    list: Arc<Mutex<Inner>>,
    node: Node,
}

impl WakerEntry {
    /// Adds this entry to the list.
    pub fn add(self: Pin<&mut Self>, waker: Waker) {
        // SAFETY: We never move the data out.
        let this = unsafe { self.get_unchecked_mut() };
        let mut inner = this.list.lock().unwrap();
        this.node.add(&mut *inner, waker);
    }
}

impl Drop for WakerEntry {
    fn drop(&mut self) {
        self.node.remove(&mut *self.list.lock().unwrap());
    }
}

// The members here must only be accessed whilst holding the mutex on the list.
struct Node {
    next: *mut Node,
    prev: *mut Node,
    waker: Option<Waker>,
}

// SAFETY: Safe because we always access all mebers of `Node` whilst holding the list lock.
unsafe impl Send for Node {}

impl Node {
    fn add(&mut self, inner: &mut Inner, waker: Waker) {
        if self.waker.is_none() {
            self.prev = std::ptr::null_mut();
            self.next = inner.head;
            inner.head = self;
            if !self.next.is_null() {
                // SAFETY: Safe because we have exclusive access to `Inner`.
                unsafe {
                    (*self.next).prev = self;
                }
            }
            inner.count += 1;
        }
        self.waker = Some(waker);
    }

    fn remove(&mut self, inner: &mut Inner) -> Option<Waker> {
        if self.waker.is_none() {
            return None;
        }
        if !self.next.is_null() {
            // SAFETY: Safe because we have exclusive access to `Inner`.
            unsafe { (*self.next).prev = self.prev };
        }
        if self.prev.is_null() {
            inner.head = self.next;
        } else {
            // SAFETY: Safe because we have exclusive access to `Inner`.
            unsafe { (*self.prev).next = self.next };
        }
        self.prev = std::ptr::null_mut();
        self.next = std::ptr::null_mut();
        inner.count -= 1;
        self.waker.take()
    }
}

/// An iterator that will drain waiters.
pub struct Drainer<'a>(MutexGuard<'a, Inner>);

impl Iterator for Drainer<'_> {
    type Item = Waker;
    fn next(&mut self) -> Option<Self::Item> {
        if self.0.head.is_null() {
            None
        } else {
            // SAFETY: Safe because we have exclusive access to `Inner`.
            unsafe { &mut (*self.0.head) }.remove(&mut self.0)
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.0.count, Some(self.0.count))
    }
}

impl ExactSizeIterator for Drainer<'_> {
    fn len(&self) -> usize {
        self.0.count
    }
}

#[cfg(all(target_os = "fuchsia", test))]
mod tests {
    use super::WakerList;
    use crate::TestExecutor;
    use assert_matches::assert_matches;
    use futures::stream::FuturesUnordered;
    use futures::task::noop_waker;
    use futures::{FutureExt, StreamExt};
    use std::future::poll_fn;
    use std::pin::pin;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::task::{Context, Poll};

    #[test]
    fn test_waker_list_can_waker_multiple_wakers() {
        let mut executor = TestExecutor::new();
        let waker_list = WakerList::new();

        static COUNT: u64 = 10;

        let counter = AtomicU64::new(0);

        // Use FuturesUnordered so that futures are only polled when explicitly woken.
        let mut futures = FuturesUnordered::new();

        for _ in 0..COUNT {
            futures.push(
                async {
                    let mut entry = pin!(waker_list.new_entry());
                    poll_fn(|cx: &mut Context<'_>| {
                        if counter.fetch_add(1, Ordering::Relaxed) < COUNT {
                            entry.as_mut().add(cx.waker().clone());
                            Poll::Pending
                        } else {
                            Poll::Ready(())
                        }
                    })
                    .await;
                }
                .boxed(),
            );
        }

        assert_eq!(executor.run_until_stalled(&mut futures.next()), Poll::Pending);

        assert_eq!(counter.load(Ordering::Relaxed), COUNT);
        assert_eq!(waker_list.len(), COUNT as usize);

        let drainer = waker_list.drain();
        assert_eq!(drainer.len(), COUNT as usize);

        for waker in drainer {
            waker.wake();
        }

        assert_matches!(
            executor.run_until_stalled(&mut futures.collect::<Vec<_>>()),
            Poll::Ready(_)
        );
        assert_eq!(counter.load(Ordering::Relaxed), COUNT * 2);
    }

    #[test]
    fn test_dropping_waker_entry_removes_from_list() {
        let waker_list = WakerList::new();

        let entry1 = pin!(waker_list.new_entry());
        entry1.add(noop_waker());

        {
            let entry2 = pin!(waker_list.new_entry());
            entry2.add(noop_waker());

            assert_eq!(waker_list.len(), 2);
        }

        assert_eq!(waker_list.len(), 1);
        assert_eq!(waker_list.drain().count(), 1);

        assert_eq!(waker_list.len(), 0);

        let entry3 = pin!(waker_list.new_entry());
        entry3.add(noop_waker());

        assert_eq!(waker_list.len(), 1);
    }

    #[test]
    fn test_waker_can_be_added_multiple_times() {
        let waker_list = WakerList::new();

        let mut entry1 = pin!(waker_list.new_entry());
        entry1.as_mut().add(noop_waker());

        let mut entry2 = pin!(waker_list.new_entry());
        entry2.as_mut().add(noop_waker());

        assert_eq!(waker_list.len(), 2);
        assert_eq!(waker_list.drain().count(), 2);
        assert_eq!(waker_list.len(), 0);

        entry1.add(noop_waker());
        entry2.add(noop_waker());

        assert_eq!(waker_list.len(), 2);
        assert_eq!(waker_list.drain().count(), 2);
        assert_eq!(waker_list.len(), 0);
    }
}
