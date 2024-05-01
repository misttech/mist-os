// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Helper types and aliases for dealing with resource references.

use core::convert::Infallible as Never;

/// The result of removing some reference-counted resource from core.
#[derive(Debug)]
pub enum RemoveResourceResult<R, D> {
    /// The resource was synchronously removed and no more references to it
    /// exist.
    Removed(R),
    /// The resource was marked for destruction but there are still references
    /// to it in existence. The provided receiver can be polled on to observe
    /// resource destruction completion.
    Deferred(D),
}

impl<R, D> RemoveResourceResult<R, D> {
    // TODO(https://fxbug.dev/336291808): Delete this method when we expose
    // deferred resources to bindings.
    pub(crate) fn unwrap_removed(self) -> R {
        match self {
            Self::Removed(r) => r,
            Self::Deferred(_) => panic!("unexpected deferred removal"),
        }
    }

    /// Maps the `Removed` variant to a different type.
    pub fn map_removed<N, F: FnOnce(R) -> N>(self, f: F) -> RemoveResourceResult<N, D> {
        match self {
            Self::Removed(r) => RemoveResourceResult::Removed(f(r)),
            Self::Deferred(d) => RemoveResourceResult::Deferred(d),
        }
    }

    /// Maps the `Deferred` variant to a different type.
    pub fn map_deferred<N, F: FnOnce(D) -> N>(self, f: F) -> RemoveResourceResult<R, N> {
        match self {
            Self::Removed(r) => RemoveResourceResult::Removed(r),
            Self::Deferred(d) => RemoveResourceResult::Deferred(f(d)),
        }
    }
}

impl<R> RemoveResourceResult<R, Never> {
    /// A helper function to unwrap a [`RemoveResourceResult`] that can never be
    /// [`RemoveResourceResult::Deferred`].
    pub fn into_removed(self) -> R {
        match self {
            Self::Removed(r) => r,
            Self::Deferred(never) => match never {},
        }
    }
}

/// An alias for [`RemoveResourceResult`] that extracts the receiver type from
/// the bindings context.
pub type RemoveResourceResultWithContext<S, BT> =
    RemoveResourceResult<S, <BT as crate::ReferenceNotifiers>::ReferenceReceiver<S>>;
