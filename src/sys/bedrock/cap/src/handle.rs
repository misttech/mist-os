// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{Capability, Remote, TryIntoOpen},
    fuchsia_zircon::{self as zx, AsHandleRef, HandleBased},
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

impl TryIntoOpen for Handle {}

/// A capability that represents a cloneable Zircon handle.
///
/// The handle is guaranteed to have ZX_RIGHT_DUPLICATE.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CloneHandle(zx::Handle);

impl Capability for CloneHandle {}

impl zx::AsHandleRef for CloneHandle {
    fn as_handle_ref(&self) -> zx::HandleRef<'_> {
        self.0.as_handle_ref()
    }
}

impl Into<zx::Handle> for CloneHandle {
    fn into(self) -> zx::Handle {
        self.0
    }
}

impl TryFrom<zx::Handle> for CloneHandle {
    type Error = zx::Status;

    fn try_from(handle: zx::Handle) -> Result<Self, Self::Error> {
        // Reject the handle if it doesn't have ZX_RIGHT_DUPLICATE.
        let info = handle.basic_info()?;
        if !info.rights.contains(zx::Rights::DUPLICATE) {
            return Err(zx::Status::BAD_HANDLE);
        }
        Ok(CloneHandle(handle))
    }
}

impl Remote for CloneHandle {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        ((*self).into(), None)
    }
}

impl TryIntoOpen for CloneHandle {}

impl Clone for CloneHandle {
    fn clone(&self) -> Self {
        // Duplicate should always succeed because CloneHandle can only be constructed from
        // a handle with ZX_RIGHT_DUPLICATE.
        Self(self.0.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("failed to duplicate handle"))
    }
}
