// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use event_listener::Event;
use fuchsia_sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// usize::MAX % EPOCHS_SIZE should be EPOCHS_SIZE - 1.
const EPOCHS_SIZE: usize = 16;

/// Epochs allows you to create a barrier that waits for all previous operations to finish.
pub struct Epochs {
    event: Event,
    inner: RwLock<Inner>,
}

struct Inner {
    first: usize, // The first epoch that we are tracking.
    last: usize,  // The last epoch that we are tracking.

    // The number of references for each epoch that we are tracking.  The appropriate entry is found
    // using epoch % EPOCHS_SIZE.
    counts: [AtomicU64; EPOCHS_SIZE],
}

impl Inner {
    fn try_wait(&mut self, epoch: usize) -> bool {
        if self.first > epoch
            || epoch == self.first && *self.counts[epoch % EPOCHS_SIZE].get_mut() == 0
        {
            return true;
        }

        if self.last == epoch {
            // We need to increment last, but we can't if the number of epochs with references is
            // EPOCHS_SIZE.  If this happens, there's a chance that new references will be taken
            // that we will then wait for, but that's OK.  When first advances all waiters get woken
            // which will allow us to try and increment last if there's now room.
            if self.last.wrapping_sub(self.first) != EPOCHS_SIZE - 1 {
                self.last = self.last.wrapping_add(1);
            }
        }

        return false;
    }
}

impl Epochs {
    /// Returns a new instance of Epochs.
    pub fn new() -> Arc<Self> {
        const INIT: AtomicU64 = AtomicU64::new(0);
        Arc::new(Self {
            event: Event::new(),
            inner: RwLock::new(Inner { first: 0, last: 0, counts: [INIT; EPOCHS_SIZE] }),
        })
    }

    /// Waits for all previous references to be released.  It's possible (under very heavy load)
    /// that this might wait for some new references too, which should be fine: what is important is
    /// that we always wait for all references prior to the barrier to finish.
    pub async fn barrier(&self) {
        let (epoch, mut listener) = {
            let mut inner = self.inner.write();
            let last = inner.last;
            let epoch = if last != inner.first && *inner.counts[last % EPOCHS_SIZE].get_mut() == 0 {
                last - 1
            } else {
                last
            };
            if inner.try_wait(epoch) {
                return;
            }
            (epoch, self.event.listen())
        };
        loop {
            listener.await;
            let mut inner = self.inner.write();
            if inner.try_wait(epoch) {
                return;
            }
            listener = self.event.listen();
        }
    }

    /// Adds a reference to the current epoch.
    pub fn add_ref(self: &Arc<Self>) -> RefGuard {
        let inner = self.inner.read();
        let epoch = inner.last;
        inner.counts[epoch % EPOCHS_SIZE].fetch_add(1, Ordering::Relaxed);
        RefGuard(self.clone(), epoch)
    }
}

pub struct RefGuard(Arc<Epochs>, usize);

impl Clone for RefGuard {
    fn clone(&self) -> Self {
        {
            let inner = self.0.inner.read();
            let count = &inner.counts[self.1 % EPOCHS_SIZE];
            count.fetch_add(1, Ordering::Relaxed);
            Self(self.0.clone(), self.1)
        }
    }
}

impl Drop for RefGuard {
    fn drop(&mut self) {
        let inner = self.0.inner.read();
        let count = &inner.counts[self.1 % EPOCHS_SIZE];
        if count.fetch_sub(1, Ordering::Relaxed) == 1
            && self.1 == inner.first
            && self.1 != inner.last
        {
            std::mem::drop(inner);
            let mut inner = self.0.inner.write();
            let mut first = inner.first.wrapping_add(1);
            while first != inner.last && *inner.counts[first % EPOCHS_SIZE].get_mut() == 0 {
                first = first.wrapping_add(1);
            }
            inner.first = first;
            std::mem::drop(inner);
            self.0.event.notify(usize::MAX);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Epochs, EPOCHS_SIZE};
    use futures::stream::{FuturesUnordered, StreamExt};
    use std::pin::pin;

    #[fuchsia::test]
    async fn test_epochs() {
        let epochs = Epochs::new();

        let guard1 = epochs.add_ref();

        let mut barrier1 = pin!(epochs.barrier());
        assert!(futures::poll!(barrier1.as_mut()).is_pending());

        // guard2 adds a reference in a different epoch.
        let guard2 = epochs.add_ref();

        let mut barrier2 = pin!(epochs.barrier());
        assert!(futures::poll!(barrier2.as_mut()).is_pending());

        // guard3 is a clone of guard1, so should be the same epoch.
        let guard3 = guard1.clone();

        std::mem::drop(guard1);
        assert!(futures::poll!(barrier1.as_mut()).is_pending());
        assert!(futures::poll!(barrier2.as_mut()).is_pending());

        // Dropping guard3 should release barrier1 but not barrier2.
        std::mem::drop(guard3);
        barrier1.await;
        assert!(futures::poll!(barrier2.as_mut()).is_pending());

        // Dropping guard2 should now unblock barrier2.
        std::mem::drop(guard2);
        barrier2.await;
    }

    #[fuchsia::test]
    async fn test_many_barriers() {
        let epochs = Epochs::new();

        let guard1 = epochs.add_ref();

        let mut futures = FuturesUnordered::new();
        for _ in 0..1000 {
            futures.push(epochs.barrier());
        }

        assert!(futures::poll!(futures.next()).is_pending());

        std::mem::drop(guard1);

        while let Some(()) = futures.next().await {}
    }

    #[fuchsia::test]
    async fn test_many_epochs() {
        let epochs = Epochs::new();

        let mut guards = Vec::new();
        let mut barriers = FuturesUnordered::new();

        // By adding a reference in each epoch, we should hit the maximum number of epochs that
        // we can track.
        for _ in 0..EPOCHS_SIZE + 1 {
            guards.push(epochs.add_ref());
            barriers.push(epochs.barrier());
            assert!(futures::poll!(barriers.next()).is_pending());
        }

        // For each guard we drop, it should release 1 barrier, except for the last two.
        for _ in 0..EPOCHS_SIZE - 1 {
            guards.remove(0);
            assert_eq!(barriers.next().await, Some(()));
            assert!(futures::poll!(barriers.next()).is_pending());
        }

        // Releasing the next guard, won't release either of the two remaining barriers.
        guards.remove(0);
        assert!(futures::poll!(barriers.next()).is_pending());

        // Releasing the last guard, should release both the two remaining barriers.
        guards.remove(0);
        assert_eq!(barriers.next().await, Some(()));
        assert_eq!(barriers.next().await, Some(()));
    }

    #[fuchsia::test]
    async fn test_out_of_order() {
        let epochs = Epochs::new();

        let guard1 = epochs.add_ref();
        let mut barrier1 = pin!(epochs.barrier());
        assert!(futures::poll!(barrier1.as_mut()).is_pending());

        let guard2 = epochs.add_ref();
        let mut barrier2 = pin!(epochs.barrier());
        assert!(futures::poll!(barrier2.as_mut()).is_pending());

        std::mem::drop(guard2);
        assert!(futures::poll!(barrier1.as_mut()).is_pending());
        assert!(futures::poll!(barrier2.as_mut()).is_pending());

        std::mem::drop(guard1);
        barrier1.await;
        barrier2.await;
    }
}
