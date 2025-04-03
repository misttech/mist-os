// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types encoding trace point identifiers.

use core::marker::PhantomData;

/// A resource identifier that can be used as an argument for trace events.
#[derive(Copy, Clone)]
pub struct TraceResourceId<'a> {
    #[cfg_attr(not(target_os = "fuchsia"), allow(unused))]
    value: u64,
    _marker: PhantomData<&'a ()>,
}

impl<'a> TraceResourceId<'a> {
    /// Creates a new resource id with the given value.
    pub fn new(value: u64) -> Self {
        Self { value, _marker: PhantomData }
    }
}

#[cfg(target_os = "fuchsia")]
impl<'a> fuchsia_trace::ArgValue for TraceResourceId<'a> {
    fn of<'x>(key: &'x str, value: Self) -> fuchsia_trace::Arg<'x>
    where
        Self: 'x,
    {
        let Self { value, _marker } = value;
        fuchsia_trace::ArgValue::of(key, value)
    }
}
