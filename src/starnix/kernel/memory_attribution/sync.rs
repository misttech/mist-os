// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::Kernel;
use crossbeam_channel::{Receiver, Sender};
use std::time::Duration;

/// Spawns a kernel thread, run the given closure with a [`Waiter`], and returns
/// the corresponding [`Notifier`].
pub(super) fn spawn_thread<F>(kernel: &Kernel, f: F) -> Notifier
where
    F: FnOnce(Waiter) + Send + 'static,
{
    let (tx, rx) = crossbeam_channel::bounded(1);

    // Don't need to use OnShutdown here because the shutdown flow will drop the thread group
    // notifier and notify waiters to exit.
    kernel.kthreads.spawn(move |_, _| {
        let (notifier, waiter) = Notifier::new();
        tx.send(notifier).unwrap();
        f(waiter);
    });

    // Receive the notifier from the spawned thread.
    rx.recv().unwrap()
}

/// Notifier is used to wake up kthread.
#[derive(Debug)]
pub struct Notifier {
    notifier: Sender<()>,
}

impl Notifier {
    /// Create a new Notifier for the given thread.
    fn new() -> (Self, Waiter) {
        let (notifier, notified) = crossbeam_channel::unbounded();
        (Self { notifier }, Waiter { notified })
    }

    /// Notify the corresponding Waiter.
    pub fn notify(&self) {
        self.notifier.send(()).ok();
    }
}

/// Waiter is used by the spawned thread to wait for notifications.
pub(super) struct Waiter {
    notified: Receiver<()>,
}

impl Waiter {
    /// Wait for a notification from [`Notifier`] by parking the thread.
    pub(super) fn wait(&self, timeout: Duration) -> Result<(), ShuttingDown> {
        self.notified.recv_timeout(timeout).map_err(|_| ShuttingDown)
    }
}

pub(super) struct ShuttingDown;
