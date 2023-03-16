// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{Capability, Remote},
    fuchsia_zircon as zx,
    futures::future::BoxFuture,
};

/// A capability that represents a Zircon handle.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Handle(zx::Handle);

impl Capability for Handle {}

impl zx::HandleBased for Handle {}

impl zx::AsHandleRef for Handle {
    fn as_handle_ref(&self) -> zx::HandleRef<'_> {
        self.0.as_handle_ref()
    }
}

impl Into<zx::Handle> for Handle {
    fn into(self) -> zx::Handle {
        self.0
    }
}

impl From<zx::Handle> for Handle {
    fn from(handle: zx::Handle) -> Self {
        Handle(handle)
    }
}

impl Remote for Handle {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        ((*self).into(), None)
    }
}
