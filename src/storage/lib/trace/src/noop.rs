// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::future::Future;

pub struct TraceFutureArgs {
    pub _use_trace_future_args: (),
}

pub trait TraceFutureExt: Future + Sized {
    fn trace(self, _args: TraceFutureArgs) -> Self {
        self
    }
}

impl<T: Future + Sized> TraceFutureExt for T {}

#[macro_export]
macro_rules! duration {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($($val),*);
    }
}

#[macro_export]
macro_rules! flow_begin {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($flow_id);
        $crate::__ignore_unused_variables!($($val),*);
    }
}

#[macro_export]
macro_rules! flow_step {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($flow_id);
        $crate::__ignore_unused_variables!($($val),*);
    }
}

#[macro_export]
macro_rules! flow_end {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($flow_id);
        $crate::__ignore_unused_variables!($($val),*);
    }
}

#[macro_export]
macro_rules! trace_future_args {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        {
            $crate::__ignore_unused_variables!($($val),*);
            $crate::TraceFutureArgs {
                _use_trace_future_args: ()
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __ignore_unused_variables {
    ($($val:expr),*) => {
        $(
            { let _ = &$val; }
        )*
    };
}
