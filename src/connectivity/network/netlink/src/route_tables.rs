// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::num::NonZeroU32;

/// The index of a route table (in netlink's view of indices).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NetlinkRouteTableIndex(u32);

impl NetlinkRouteTableIndex {
    pub(crate) const fn new(index: u32) -> Self {
        Self(index)
    }

    pub(crate) const fn get(self) -> u32 {
        let Self(index) = self;
        index
    }
}

/// The index of a route table (in netlink's view of indices).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NonZeroNetlinkRouteTableIndex(NonZeroU32);

impl NonZeroNetlinkRouteTableIndex {
    pub(crate) const fn new(index: NetlinkRouteTableIndex) -> Option<Self> {
        let NetlinkRouteTableIndex(index) = index;
        // NB: `Option::map` is not available in `const`
        match NonZeroU32::new(index) {
            None => None,
            Some(index) => Some(Self(index)),
        }
    }

    pub(crate) const fn new_non_zero(index: NonZeroU32) -> Self {
        Self(index)
    }

    pub(crate) const fn get(self) -> NonZeroU32 {
        let Self(index) = self;
        index
    }
}

impl From<NonZeroNetlinkRouteTableIndex> for NetlinkRouteTableIndex {
    fn from(index: NonZeroNetlinkRouteTableIndex) -> Self {
        Self(index.get().get())
    }
}
