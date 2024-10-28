// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;

/// A resource associated with an endpoint.
#[repr(transparent)]
pub struct EndpointResource<T, P> {
    resource: T,
    _endpoint: PhantomData<P>,
}

impl<T, P> EndpointResource<T, P> {
    /// Returns a new endpoint over the given resource.
    pub fn new(resource: T) -> Self {
        Self { resource, _endpoint: PhantomData }
    }

    /// Returns the underlying resource.
    pub fn into_inner(self) -> T {
        self.resource
    }
}
