// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
pub mod fake {
    use crate::api;
    use crate::hash::fake::Hash;
    use std::iter;

    /// TODO(fxbug.dev/111249): Implement production package resolver API.
    #[derive(Default)]
    pub(crate) struct PackageResolver;

    impl api::PackageResolver for PackageResolver {
        type Hash = Hash;

        fn resolve(&self, _url: api::PackageResolverUrl) -> Option<Self::Hash> {
            None
        }

        fn aliases(&self, _hash: Self::Hash) -> Box<dyn Iterator<Item = api::PackageResolverUrl>> {
            Box::new(iter::empty())
        }
    }
}
