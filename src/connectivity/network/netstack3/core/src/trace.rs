// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Convenience macros for using a [`crate::context::TracingContext`].

/// Convenience wrapper around the [`crate::context::TracingContext::duration`]
/// trait method.
///
/// [`crate::context::TracingContext::duration`] uses RAII to begin and end the
/// duration by tying the scope of the duration to the lifetime of the object it
/// returns. This macro encapsulates that logic such that the trace duration
/// will end when the scope in which the macro is called ends.
macro_rules! trace_duration {
    ($ctx:ident, $name:expr) => {
        let _scope = $ctx.duration(::cstr::cstr!($name));
    };
}

pub(crate) use trace_duration;
