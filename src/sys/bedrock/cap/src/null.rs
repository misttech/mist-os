// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{Capability, Remote},
    fuchsia_zircon as zx,
    futures::future::BoxFuture,
};

/// An empty capability that represents nothing.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Null {}

impl Capability for Null {}

impl Into<zx::Handle> for Null {
    fn into(self) -> zx::Handle {
        zx::Handle::invalid()
    }
}

impl Remote for Null {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        (zx::Handle::invalid(), None)
    }
}

#[cfg(test)]
mod tests {
    use {crate::null::*, fuchsia_zircon as zx};

    #[test]
    fn into_handle() {
        let null = Null {};
        let null_handle: zx::Handle = null.into();
        assert_eq!(zx::Handle::invalid(), null_handle);
    }
}
