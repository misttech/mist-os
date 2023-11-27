// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{ffi::CStr, future::Future};

pub use cstr::cstr;
#[cfg(feature = "tracing")]
pub use fuchsia_trace;
pub use fxfs_trace_macros::*;

#[cfg(feature = "tracing")]
pub trait FxfsTraceFutureExt: Future + Sized {
    fn trace(self, name: &'static CStr) -> fuchsia_trace::TraceFuture<Self> {
        fuchsia_trace::TraceFuture::new(cstr::cstr!("fxfs"), name, fuchsia_trace::Id::new(), self)
    }
}

impl<T: Future + Sized> FxfsTraceFutureExt for T {}

#[cfg(feature = "tracing")]
#[macro_export]
macro_rules! duration {
    ($name:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::duration!("fxfs", $name $(,$key => $val)*);
    }
}

#[cfg(feature = "tracing")]
#[macro_export]
macro_rules! instant {
    ($name:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::instant!(
            "fxfs",
            $name,
            $crate::fuchsia_trace::Scope::Thread $(,$key => $val)*
        );
    }
}

#[cfg(feature = "tracing")]
#[macro_export]
macro_rules! flow_begin {
    ($name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::flow_begin!("fxfs", $name, ($flow_id).into() $(,$key => $val)*);
    }
}

#[cfg(feature = "tracing")]
#[macro_export]
macro_rules! flow_step {
    ($name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::flow_step!("fxfs", $name, ($flow_id).into() $(,$key => $val)*);
    }
}

#[cfg(feature = "tracing")]
#[macro_export]
macro_rules! flow_end {
    ($name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::flow_end!("fxfs", $name, ($flow_id).into() $(,$key => $val)*);
    }
}

#[cfg(not(feature = "tracing"))]
pub trait FxfsTraceFutureExt: Future + Sized {
    fn trace(self, _name: &'static CStr) -> Self {
        self
    }
}

#[cfg(not(feature = "tracing"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __ignore_unused_variables {
    ($($val:expr),*) => {
        $(
            { let _ = &$val; }
        )*
    };
}

#[cfg(not(feature = "tracing"))]
#[macro_export]
macro_rules! duration {
    ($name:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($($val),*);
    }
}

#[cfg(not(feature = "tracing"))]
#[macro_export]
macro_rules! instant {
    ($name:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($($val),*);
    }
}

#[cfg(not(feature = "tracing"))]
#[macro_export]
macro_rules! flow_begin {
    ($name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($flow_id);
        $crate::__ignore_unused_variables!($($val),*);
    }
}

#[cfg(not(feature = "tracing"))]
#[macro_export]
macro_rules! flow_step {
    ($name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($flow_id);
        $crate::__ignore_unused_variables!($($val),*);
    }
}

#[cfg(not(feature = "tracing"))]
#[macro_export]
macro_rules! flow_end {
    ($name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($flow_id);
        $crate::__ignore_unused_variables!($($val),*);
    }
}
