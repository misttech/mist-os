// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Defines witness types for the Bindings route table implementation.

use core::marker::PhantomData;

use net_types::ip::{GenericOverIp, Ip, IpVersion, Ipv4, Ipv6};

/// Uniquely identifies a route table.
///
/// The type is a witness for the ID being unique among v4 and v6 tables.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, GenericOverIp)]
#[generic_over_ip(I, Ip)]
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

    /// Returns the next table ID. [`None`] if overflows.
    pub fn next(&self) -> Option<Self> {
        self.0.checked_add(2).map(|x| TableId(x, PhantomData))
    }
}

impl<I: Ip> From<TableId<I>> for u32 {
    fn from(TableId(id, _marker): TableId<I>) -> u32 {
        id
    }
}

pub(crate) const IPV4_MAIN_TABLE_ID: TableId<Ipv4> =
    const_unwrap::const_unwrap_option(TableId::new(0));
pub(crate) const IPV6_MAIN_TABLE_ID: TableId<Ipv6> =
    const_unwrap::const_unwrap_option(TableId::new(1));

/// Returns the main table ID for the IP version
pub(crate) fn main_table_id<I: Ip>() -> TableId<I> {
    I::map_ip((), |()| IPV4_MAIN_TABLE_ID, |()| IPV6_MAIN_TABLE_ID)
}
