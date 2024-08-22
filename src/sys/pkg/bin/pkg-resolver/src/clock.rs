// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

#[cfg(not(test))]
pub(crate) fn now() -> zx::MonotonicTime {
    zx::MonotonicTime::get()
}

#[cfg(test)]
pub(crate) use mock::now;

#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use std::cell::Cell;

    thread_local!(
        static MOCK_TIME: Cell<zx::MonotonicTime> = Cell::new(zx::MonotonicTime::get())
    );

    pub fn now() -> zx::MonotonicTime {
        MOCK_TIME.with(|time| time.get())
    }

    pub fn set(new_time: zx::MonotonicTime) {
        MOCK_TIME.with(|time| time.set(new_time));
    }
}
