// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Noop implementations of the functions in `fuchsia_trace` that are called from the tracing
//! macros. These functions are used instead of omitting the calls within the macro to ensure that
//! the macro invocations still compile even when tracing is disabled. The tests in this library are
//! built with tracing enabled and with tracing disabled which will ensure that these functions
//! can't diverge from the real ones.

use std::{ffi::CStr, future::Future, marker::PhantomData};

pub struct TraceCategoryContext(());

impl TraceCategoryContext {
    pub fn acquire(_category: &'static CStr) -> Option<Self> {
        None
    }
}

pub struct DurationScope<'a>(PhantomData<&'a ()>);

#[inline]
pub const fn duration<'a>(
    _category: &'static CStr,
    _name: &'static CStr,
    _args: &'a [Arg<'_>],
) -> DurationScope<'a> {
    DurationScope(PhantomData)
}

pub struct Id(());

impl From<u64> for Id {
    #[inline]
    fn from(_id: u64) -> Self {
        Self(())
    }
}

#[inline]
pub const fn flow_begin(
    _context: &TraceCategoryContext,
    _name: &'static CStr,
    _flow_id: Id,
    _args: &[Arg<'_>],
) {
}

#[inline]
pub const fn flow_step(
    _context: &TraceCategoryContext,
    _name: &'static CStr,
    _flow_id: Id,
    _args: &[Arg<'_>],
) {
}

#[inline]
pub const fn flow_end(
    _context: &TraceCategoryContext,
    _name: &'static CStr,
    _flow_id: Id,
    _args: &[Arg<'_>],
) {
}

pub struct TraceFutureArgs {
    pub _use_trace_future_args: (),
}

pub trait TraceFutureExt: Future + Sized {
    fn trace(self, _args: TraceFutureArgs) -> Self {
        self
    }
}

impl<T: Future + Sized> TraceFutureExt for T {}

#[inline]
pub fn trace_future_args<'a>(
    _context: Option<TraceCategoryContext>,
    _category: &'static CStr,
    _name: &'static CStr,
    _args: Vec<Arg<'a>>,
) -> TraceFutureArgs {
    TraceFutureArgs { _use_trace_future_args: () }
}

pub struct Arg<'a>(PhantomData<&'a ()>);

pub trait ArgValue {
    fn of<'a>(key: &'a str, value: Self) -> Arg<'a>
    where
        Self: 'a;
}

macro_rules! impl_arg_value {
    ($($type:ty),*) => {
        $(
            impl ArgValue for $type {
                #[inline]
                fn of<'a>(_key: &'a str, _value: Self) -> Arg<'a>
                where
                    Self: 'a,
                {
                    Arg(PhantomData)
                }
            }
        )*
    };
}

impl_arg_value!((), bool, i32, u32, i64, u64, isize, usize, f64);

macro_rules! impl_generic_arg_value {
    ($(($type:ty, $generics:tt)),*) => {
        $(
        impl<$generics> ArgValue for $type {
            #[inline]
            fn of<'a>(_key: &'a str, _value: Self) -> Arg<'a>
            where
                Self: 'a,
            {
                Arg(PhantomData)
            }
        }
    )*
    };
}

impl_generic_arg_value!((*const T, T), (*mut T, T), (&'b str, 'b));
