// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;

/// A strongly typed service instance.
#[derive(Debug)]
#[repr(transparent)]
pub struct ServiceInstance<I, S> {
    instance: I,
    _service: PhantomData<S>,
}

impl<I, S> ServiceInstance<I, S> {
    /// Returns a new service instance over the given instance.
    pub fn from_untyped(instance: I) -> Self {
        Self { instance, _service: PhantomData }
    }

    /// Returns the underlying instance.
    pub fn into_untyped(self) -> I {
        self.instance
    }

    /// Returns a reference to the underlying instance.
    pub fn as_untyped_mut(&mut self) -> &mut I {
        &mut self.instance
    }
}
