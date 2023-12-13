// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use fuchsia_trace;
pub use fuchsia_trace::TraceFutureExt;

#[macro_export]
macro_rules! duration {
    ($name:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::duration!("fxfs", $name $(,$key => $val)*);
    }
}

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

#[macro_export]
macro_rules! flow_begin {
    ($name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::flow_begin!("fxfs", $name, ($flow_id).into() $(,$key => $val)*);
    }
}

#[macro_export]
macro_rules! flow_step {
    ($name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::flow_step!("fxfs", $name, ($flow_id).into() $(,$key => $val)*);
    }
}

#[macro_export]
macro_rules! flow_end {
    ($name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::flow_end!("fxfs", $name, ($flow_id).into() $(,$key => $val)*);
    }
}

#[macro_export]
macro_rules! trace_future_args {
    ($name:expr $(, $key:expr => $val:expr)*) => {
        $crate::fuchsia_trace::trace_future_args!("fxfs", $name $(,$key => $val)*);
    };
}
