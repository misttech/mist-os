// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fuchsia_trace::{Arg, TraceFutureArgs},
    std::{ffi::CStr, vec::Vec},
};

pub use fuchsia_trace::{
    duration, flow_begin, flow_end, flow_step, ArgValue, Id, TraceCategoryContext, TraceFutureExt,
};

#[inline]
pub fn trace_future_args<'a>(
    context: Option<TraceCategoryContext>,
    category: &'static CStr,
    name: &'static CStr,
    args: Vec<Arg<'a>>,
) -> TraceFutureArgs<'a> {
    TraceFutureArgs { category, name, context, args, flow_id: None, _use_trace_future_args: () }
}
