// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{ffi::CStr, future::Future};

pub trait FxfsTraceFutureExt: Future + Sized {
    fn trace(self, _name: &'static CStr) -> Self {
        self
    }
}

#[macro_export]
macro_rules! duration {
    ($name:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($($val),*);
    }
}

#[macro_export]
macro_rules! instant {
    ($name:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($($val),*);
    }
}

#[macro_export]
macro_rules! flow_begin {
    ($name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($flow_id);
        $crate::__ignore_unused_variables!($($val),*);
    }
}

#[macro_export]
macro_rules! flow_step {
    ($name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($flow_id);
        $crate::__ignore_unused_variables!($($val),*);
    }
}

#[macro_export]
macro_rules! flow_end {
    ($name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::__ignore_unused_variables!($flow_id);
        $crate::__ignore_unused_variables!($($val),*);
    }
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
