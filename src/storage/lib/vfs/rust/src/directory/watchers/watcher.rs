// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A task that is run to process communication with an individual watcher.

use crate::{
    directory::{entry_container::DirectoryWatcher, watchers::event_producers::EventProducer},
    execution_scope::ExecutionScope,
};

use {
    fidl_fuchsia_io as fio,
    futures::{
        channel::mpsc::{self, UnboundedSender},
        select, FutureExt,
    },
};

#[cfg(not(target_os = "fuchsia"))]
use fuchsia_async::emulated_handle::MessageBuf;
#[cfg(target_os = "fuchsia")]
use fuchsia_zircon::MessageBuf;

/// `done` is not guaranteed to be called if the task failed to start.  It should only happen
/// in case the return value is an `Err`.  Unfortunately, there is no way to return the `done`
/// object itself, as the [`futures::Spawn::spawn_obj`] does not return the ownership in case
/// of a failure.
pub(crate) fn new(
    scope: ExecutionScope,
    mask: fio::WatchMask,
    watcher: DirectoryWatcher,
    done: impl FnOnce() + Send + 'static,
) -> Controller {
    use futures::StreamExt as _;

    let (sender, mut receiver) = mpsc::unbounded::<Vec<u8>>();

    let task = async move {
        let _done = CallOnDrop(Some(done));
        let mut buf = MessageBuf::new();
        let mut recv_msg = watcher.channel().recv_msg(&mut buf).fuse();
        loop {
            select! {
                command = receiver.next() => match command {
                    Some(message) => {
                        let result = watcher.channel().write(&*message, &mut []);
                        if result.is_err() {
                            break;
                        }
                    },
                    None => break,
                },
                _ = recv_msg => {
                    // We do not expect any messages to be received over the watcher connection.
                    // Should we receive a message we will close the connection to indicate an
                    // error.  If any error occurs, we also close the connection.  And if the
                    // connection is closed, we just stop the command processing as well.
                    break;
                },
            }
        }
    };

    scope.spawn(task);
    Controller { mask, messages: sender }
}

pub struct Controller {
    mask: fio::WatchMask,
    messages: UnboundedSender<Vec<u8>>,
}

impl Controller {
    /// Sends a buffer to the connected watcher.  `mask` specifies the type of the event the buffer
    /// is for.  If the watcher mask does not include the event specified by the `mask` then the
    /// buffer is not sent and `buffer` is not even invoked.
    pub(crate) fn send_buffer(&self, mask: fio::WatchMask, buffer: impl FnOnce() -> Vec<u8>) {
        if !self.mask.intersects(mask) {
            return;
        }

        if self.messages.unbounded_send(buffer()).is_ok() {
            return;
        }

        // An error to send indicates the execution task has been disconnected.  Controller should
        // always be removed from the watchers list before it is destroyed.  So this is some
        // logical bug.
        debug_assert!(false, "Watcher controller failed to send a command to the watcher.");
    }

    /// Uses a `producer` to generate one or more buffers and send them all to the connected
    /// watcher.  `producer.mask()` is used to determine the type of the event - in case the
    /// watcher mask does not specify that it needs to receive this event, then the producer is not
    /// used and `false` is returned.  If the producers mask and the watcher mask overlap, then
    /// `true` is returned (even if the producer did not generate a single buffer).
    pub fn send_event(&self, producer: &mut dyn EventProducer) -> bool {
        if !self.mask.intersects(producer.mask()) {
            return false;
        }

        while producer.prepare_for_next_buffer() {
            let buffer = producer.buffer();
            if self.messages.unbounded_send(buffer).is_ok() {
                continue;
            }

            // An error to send indicates the execution task has been disconnected.  Controller
            // should always be removed from the watchers list before it is destroyed.  So this is
            // some logical bug.
            debug_assert!(false, "Watcher controller failed to send a command to the watcher.");
        }

        return true;
    }
}

/// Calls the function when this object is dropped.
struct CallOnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for CallOnDrop<F> {
    fn drop(&mut self) {
        self.0.take().unwrap()();
    }
}
