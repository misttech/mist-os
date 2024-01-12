// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use fuchsia_trace;
pub use fuchsia_trace::TraceFutureExt;

#[macro_export]
macro_rules! duration {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::duration!($category, $name $(,$key => $val)*);
    }
}

#[macro_export]
macro_rules! flow_begin {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::flow_begin!($category, $name, ($flow_id).into() $(,$key => $val)*);
    }
}

#[macro_export]
macro_rules! flow_step {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::flow_step!($category, $name, ($flow_id).into() $(,$key => $val)*);
    }
}

#[macro_export]
macro_rules! flow_end {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::flow_end!($category, $name, ($flow_id).into() $(,$key => $val)*);
    }
}

#[macro_export]
macro_rules! trace_future_args {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::trace_future_args!($category, $name $(,$key => $val)*)
    }
}
