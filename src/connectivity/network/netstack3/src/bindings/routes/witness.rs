// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Defines witness types for the Bindings route table implementation.

use core::marker::PhantomData;

use net_types::ip::{Ip, IpVersion};

/// Uniquely identifies a route table.
///
/// The type is a witness for the ID being unique among v4 and v6 tables.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TableId<I: Ip>(u32, PhantomData<I>);

impl<I: Ip> TableId<I> {
    pub(crate) const fn new(id: u32) -> Option<Self> {
        // TableIds are unique in all route tables, as an implementation detail,
        // we use even numbers for v4 tables and odd numbers for v6 tables.
        if match I::VERSION {
            IpVersion::V4 => id % 2 == 0,
            IpVersion::V6 => id % 2 == 1,
        } {
            Some(Self(id, PhantomData))
        } else {
            None
        }
    }
}

impl<I: Ip> From<TableId<I>> for u32 {
    fn from(TableId(id, _marker): TableId<I>) -> u32 {
        id
    }
}
