// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/111250): Implement production component resolver API.

#[cfg(test)]
pub mod fake {
    use crate::api;
    use crate::hash::fake::Hash;
    use std::iter;

    #[derive(Default)]
    pub(crate) struct ComponentResolver;

    impl api::ComponentResolver for ComponentResolver {
        type Hash = Hash;

        fn resolve(&self, _url: api::ComponentResolverUrl) -> Option<Self::Hash> {
            None
        }

        fn aliases(
            &self,
            _hash: Self::Hash,
        ) -> Box<dyn Iterator<Item = api::ComponentResolverUrl>> {
            Box::new(iter::empty())
        }
    }
}
