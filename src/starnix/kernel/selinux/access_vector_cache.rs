// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{AccessQueryable, AccessVector, DenyAll, ObjectClass, SecurityId};

/// A cache of access vectors that associate source SID, target SID, and object type to a set of
/// rights permitted for the source accessing the target object.
pub trait AccessVectorCache: AccessQueryable {
    /// Removes all entries from this cache and any delegate caches encapsulated in this cache.
    fn reset(&mut self);
}

impl AccessVectorCache for DenyAll {
    /// A no-op implementation: [`DenyAll`] has no state to reset and no delegates to notify
    /// when it is being treated as a cache to be reset.
    fn reset(&mut self) {}
}

/// An empty access vector cache that delegates to an [`AccessQueryable`].
#[derive(Default)]
pub struct EmptyAccessVectorCache<D: AccessVectorCache = DenyAll> {
    delegate: D,
}

impl<D: AccessVectorCache> EmptyAccessVectorCache<D> {
    /// Constructs an empty access vector cache that delegates to `delegate`.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn new(delegate: D) -> Self {
        Self { delegate }
    }
}

impl<D: AccessVectorCache> AccessQueryable for EmptyAccessVectorCache<D> {
    fn query(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector {
        self.delegate.query(source_sid, target_sid, target_class)
    }
}

impl<D: AccessVectorCache> AccessVectorCache for EmptyAccessVectorCache<D> {
    fn reset(&mut self) {
        self.delegate.reset()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn empty_access_vector_cache_default_deny_all() {
        let mut avc = EmptyAccessVectorCache::<DenyAll>::default();
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
    }
}
