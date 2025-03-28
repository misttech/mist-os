// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::Cell;
use std::marker::PhantomData;
use std::thread::{self, Thread};

use crate::task::Kernel;

/// Spawns a kernel thread, run the given closure with a [`Waiter`], and returns
/// the corresponding [`Notifier`].
pub(super) fn spawn_thread<F>(kernel: &Kernel, f: F) -> Notifier
where
    F: FnOnce(Waiter) + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();

    kernel.kthreads.spawn(move |_, _| {
        // Send the thread handle to be parked and unparked.
        let thread = thread::current();
        tx.send(Notifier::new(thread)).unwrap();
        let key = Key(PhantomData);
        let waiter = Waiter(key);
        f(waiter);
    });

    // Receive the notifier from the spawned thread.
    rx.recv().unwrap()
}

/// Notifier is used to wake up a specific thread.
#[derive(Debug)]
pub struct Notifier {
    thread: Thread,
}

impl Notifier {
    /// Create a new Notifier for the given thread.
    fn new(thread: Thread) -> Self {
        Notifier { thread }
    }

    /// Notify the associated thread by unparking it.
    pub fn notify(&self) {
        self.thread.unpark();
    }
}

/// Waiter is used by the spawned thread to wait for notifications.
pub(super) struct Waiter(Key);

struct Key(PhantomUnsync);

/// A type that cannot be sent across threads. This disables sending
/// [`Waiter`] to other threads which wouldn't make sense.
type PhantomUnsync = PhantomData<Cell<()>>;

impl Waiter {
    /// Wait for a notification from [`Notifier`] by parking the thread.
    pub(super) fn wait(&self, timeout: std::time::Duration) {
        thread::park_timeout(timeout);
    }
}
