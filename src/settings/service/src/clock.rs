// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::MonotonicInstant;

const TIMESTAMP_DIVIDEND: i64 = 1_000_000_000;

#[cfg(not(test))]
pub(crate) fn now() -> MonotonicInstant {
    MonotonicInstant::get()
}

#[cfg(not(test))]
pub(crate) fn inspect_format_now() -> String {
    // follows syslog timestamp format: [seconds.nanos]
    let timestamp = now().into_nanos();
    let seconds = timestamp / TIMESTAMP_DIVIDEND;
    let nanos = timestamp % TIMESTAMP_DIVIDEND;
    format!("{seconds}.{nanos:09}")
}

#[cfg(test)]
pub(crate) use mock::now;

#[cfg(test)]
pub(crate) use mock::inspect_format_now;

#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use std::cell::RefCell;

    thread_local!(static MOCK_TIME: RefCell<MonotonicInstant> = RefCell::new(MonotonicInstant::get()));

    pub(crate) fn now() -> MonotonicInstant {
        MOCK_TIME.with(|time| *time.borrow())
    }

    pub(crate) fn set(new_time: MonotonicInstant) {
        MOCK_TIME.with(|time| *time.borrow_mut() = new_time);
    }

    pub(crate) fn inspect_format_now() -> String {
        let timestamp = now().into_nanos();
        let seconds = timestamp / TIMESTAMP_DIVIDEND;
        let nanos = timestamp % TIMESTAMP_DIVIDEND;
        format!("{seconds}.{nanos:09}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn test_inspect_format() {
        mock::set(MonotonicInstant::from_nanos(0));
        assert_eq!(String::from("0.000000000"), mock::inspect_format_now());

        mock::set(MonotonicInstant::from_nanos(123));
        assert_eq!(String::from("0.000000123"), mock::inspect_format_now());

        mock::set(MonotonicInstant::from_nanos(123_000_000_000));
        assert_eq!(String::from("123.000000000"), mock::inspect_format_now());

        mock::set(MonotonicInstant::from_nanos(123_000_000_123));
        assert_eq!(String::from("123.000000123"), mock::inspect_format_now());

        mock::set(MonotonicInstant::from_nanos(123_001_230_000));
        assert_eq!(String::from("123.001230000"), mock::inspect_format_now());
    }
}
