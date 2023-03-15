// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use downcast_rs::{impl_downcast, Downcast};

/// The capability trait, implemented by all capabilities.
pub trait Capability: Downcast + Send + Sync {}
impl_downcast!(Capability);

/// Trait object used to hold any kind of capability.
pub type AnyCapability = Box<dyn Capability>;

impl std::fmt::Debug for AnyCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Capability").field(&self.as_any().type_id()).finish()
    }
}
