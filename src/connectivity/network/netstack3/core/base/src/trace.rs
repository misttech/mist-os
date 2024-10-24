// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common execution trace abstractions.
//!
//! Tracing is abstracted in core crates so it can be tied to the fuchsia
//! tracing subsystem in bindings without taking Fuchsia-specific dependencies.

use core::ffi::CStr;

/// Convenience wrapper around the [`TracingContext::duration`] trait method.
///
/// [`TracingContext::duration`] uses RAII to begin and end the duration by
/// tying the scope of the duration to the lifetime of the object it returns.
/// This macro encapsulates that logic such that the trace duration will end
/// when the scope in which the macro is called ends.
#[macro_export]
macro_rules! trace_duration {
    ($ctx:ident, $name:literal) => {
        let _scope = $crate::TracingContext::duration($ctx, $name);
    };
}

/// A context for emitting tracing data.
// TODO(https://fxbug.dev/338642329): Change this API to not take CStr when
// tracing in Fuchsia doesn't require null terminated strings.
pub trait TracingContext {
    /// The scope of a trace duration.
    ///
    /// Its lifetime corresponds to the beginning and end of the duration.
    type DurationScope;

    /// Writes a duration event which ends when the returned scope is dropped.
    ///
    /// Durations describe work which is happening synchronously on one thread.
    /// Care should be taken to avoid a duration's scope spanning an `await`
    /// point in asynchronous code.
    fn duration(&self, name: &'static CStr) -> Self::DurationScope;
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use super::*;

    /// A fake [`TracingContext`].
    #[derive(Default)]
    pub struct FakeTracingCtx;

    impl TracingContext for FakeTracingCtx {
        type DurationScope = ();

        fn duration(&self, _: &'static core::ffi::CStr) {}
    }
}
