// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{sys, HandleRef, MonotonicInstant, Signals, Status};

/// A "wait item" containing a handle reference and information about what signals
/// to wait on, and, on return from `object_wait_many`, which are pending.
#[repr(C)]
#[derive(Debug)]
pub struct WaitItem<'a> {
    /// The handle to wait on.
    pub handle: HandleRef<'a>,
    /// A set of signals to wait for.
    pub waitfor: Signals,
    /// The set of signals pending, on return of `object_wait_many`.
    pub pending: Signals,
}

/// Wait on multiple handles.
/// The success return value is a bool indicating whether one or more of the
/// provided handle references was closed during the wait.
///
/// Wraps the
/// [zx_object_wait_many](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_wait_many.md)
/// syscall.
pub fn object_wait_many(
    items: &mut [WaitItem<'_>],
    deadline: MonotonicInstant,
) -> Result<bool, Status> {
    let items_ptr = items.as_mut_ptr().cast::<sys::zx_wait_item_t>();
    let status = unsafe { sys::zx_object_wait_many(items_ptr, items.len(), deadline.into_nanos()) };
    if status == sys::ZX_ERR_CANCELED {
        return Ok(true);
    }
    Status::ok(status).map(|()| false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AsHandleRef, Duration, Event, WaitResult};

    #[test]
    fn wait_and_signal() {
        let event = Event::create();
        let ten_ms = Duration::from_millis(10);

        // Waiting on it without setting any signal should time out.
        assert_eq!(
            event.wait_handle(Signals::USER_0, MonotonicInstant::after(ten_ms)),
            WaitResult::TimedOut(Signals::empty())
        );

        // If we set a signal, we should be able to wait for it.
        assert!(event.signal_handle(Signals::NONE, Signals::USER_0).is_ok());
        assert_eq!(
            event.wait_handle(Signals::USER_0, MonotonicInstant::after(ten_ms)).unwrap(),
            Signals::USER_0
        );

        // Should still work, signals aren't automatically cleared.
        assert_eq!(
            event.wait_handle(Signals::USER_0, MonotonicInstant::after(ten_ms)).unwrap(),
            Signals::USER_0
        );

        // Now clear it, and waiting should time out again.
        assert!(event.signal_handle(Signals::USER_0, Signals::NONE).is_ok());
        assert_eq!(
            event.wait_handle(Signals::USER_0, MonotonicInstant::after(ten_ms)),
            WaitResult::TimedOut(Signals::empty())
        );
    }

    #[test]
    fn wait_many_and_signal() {
        let ten_ms = Duration::from_millis(10);
        let e1 = Event::create();
        let e2 = Event::create();

        // Waiting on them now should time out.
        let mut items = vec![
            WaitItem {
                handle: e1.as_handle_ref(),
                waitfor: Signals::USER_0,
                pending: Signals::NONE,
            },
            WaitItem {
                handle: e2.as_handle_ref(),
                waitfor: Signals::USER_1,
                pending: Signals::NONE,
            },
        ];
        assert_eq!(
            object_wait_many(&mut items, MonotonicInstant::after(ten_ms)),
            Err(Status::TIMED_OUT)
        );
        assert_eq!(items[0].pending, Signals::NONE);
        assert_eq!(items[1].pending, Signals::NONE);

        // Signal one object and it should return success.
        assert!(e1.signal_handle(Signals::NONE, Signals::USER_0).is_ok());
        assert!(object_wait_many(&mut items, MonotonicInstant::after(ten_ms)).is_ok());
        assert_eq!(items[0].pending, Signals::USER_0);
        assert_eq!(items[1].pending, Signals::NONE);

        // Signal the other and it should return both.
        assert!(e2.signal_handle(Signals::NONE, Signals::USER_1).is_ok());
        assert!(object_wait_many(&mut items, MonotonicInstant::after(ten_ms)).is_ok());
        assert_eq!(items[0].pending, Signals::USER_0);
        assert_eq!(items[1].pending, Signals::USER_1);

        // Clear signals on both; now it should time out again.
        assert!(e1.signal_handle(Signals::USER_0, Signals::NONE).is_ok());
        assert!(e2.signal_handle(Signals::USER_1, Signals::NONE).is_ok());
        assert_eq!(
            object_wait_many(&mut items, MonotonicInstant::after(ten_ms)),
            Err(Status::TIMED_OUT)
        );
        assert_eq!(items[0].pending, Signals::NONE);
        assert_eq!(items[1].pending, Signals::NONE);
    }
}
