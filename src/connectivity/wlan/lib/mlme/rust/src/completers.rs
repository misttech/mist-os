// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

/// Type that wraps a closure for completing a DFv2 Start operation.
///
/// Calling `StartCompleter::reply()` forwards the status to the wrapped closure. Otherwise,
/// dropping a `StartCompleter` indicates Start failed with a `zx::Status::BAD_STATE` status.
pub struct StartCompleter<F>
where
    F: FnOnce(zx::sys::zx_status_t),
{
    completer: Option<F>,
}

impl<F> StartCompleter<F>
where
    F: FnOnce(zx::sys::zx_status_t),
{
    pub fn new(completer: F) -> Self {
        Self { completer: Some(completer) }
    }

    pub fn reply(mut self, status: Result<(), zx::Status>) {
        let completer = match self.completer.take() {
            None => unreachable!(),
            Some(completer) => completer,
        };
        completer(zx::Status::from(status).into_raw())
    }
}

impl<F> Drop for StartCompleter<F>
where
    F: FnOnce(zx::sys::zx_status_t),
{
    fn drop(&mut self) {
        if let Some(completer) = self.completer.take() {
            completer(zx::Status::BAD_STATE.into_raw())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::channel::oneshot;

    #[test]
    fn reply_with_ok() {
        let (sender, mut receiver) = oneshot::channel::<zx::sys::zx_status_t>();
        let start_completer = StartCompleter::new(move |status| {
            sender.send(status).expect("Failed to send result.");
        });
        start_completer.reply(Ok(()));
        assert_eq!(Ok(Some(zx::Status::OK.into_raw())), receiver.try_recv());
    }

    #[test]
    fn reply_with_error() {
        let (sender, mut receiver) = oneshot::channel::<zx::sys::zx_status_t>();
        let start_completer = StartCompleter::new(move |status| {
            sender.send(status).expect("Failed to send result.");
        });
        start_completer.reply(Err(zx::Status::NO_RESOURCES));
        assert_eq!(Ok(Some(zx::Status::NO_RESOURCES.into_raw())), receiver.try_recv());
    }

    #[test]
    fn reply_with_error_when_dropped() {
        let (sender, mut receiver) = oneshot::channel::<zx::sys::zx_status_t>();
        let start_completer = StartCompleter::new(move |status| {
            sender.send(status).expect("Failed to send result.");
        });
        drop(start_completer);
        assert_eq!(Ok(Some(zx::Status::BAD_STATE.into_raw())), receiver.try_recv());
    }
}
