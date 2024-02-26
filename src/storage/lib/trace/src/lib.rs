// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(feature = "tracing")]
mod fuchsia;
#[cfg(feature = "tracing")]
pub mod __backend {
    pub use crate::fuchsia::*;
}

#[cfg(not(feature = "tracing"))]
mod noop;
#[cfg(not(feature = "tracing"))]
pub mod __backend {
    pub use crate::noop::*;
}

pub use __backend::{Id, TraceFutureExt};

/// Convenience macro for creating a trace duration event from this macro invocation to the end of
/// the current scope.
///
/// See `fuchsia_trace::duration!` for more details.
#[macro_export]
macro_rules! duration {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        let args;
        let _scope = if $crate::__backend::TraceCategoryContext::acquire($category).is_some() {
            args = [$($crate::__backend::ArgValue::of($key, $val)),*];
            Some($crate::__backend::duration($category, $name, &args))
        } else {
            None
        };
    }
}

/// Writes a flow begin event with the specified id.
///
/// See `fuchsia_trace::flow_begin!` for more details.
#[macro_export]
macro_rules! flow_begin {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        if let Some(context) = $crate::__backend::TraceCategoryContext::acquire($category) {
            $crate::__backend::flow_begin(&context, $name, ($flow_id).into(),
                                          &[$($crate::__backend::ArgValue::of($key, $val)),*])
        }
    }
}

/// Writes a flow step event with the specified id.
///
/// See `fuchsia_trace::flow_step!` for more details.
#[macro_export]
macro_rules! flow_step {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        if let Some(context) = $crate::__backend::TraceCategoryContext::acquire($category) {
            $crate::__backend::flow_step(&context, $name, ($flow_id).into(),
                                         &[$($crate::__backend::ArgValue::of($key, $val)),*])
        }
    }
}

/// Writes a flow end event with the specified id.
///
/// See `fuchsia_trace::flow_end!` for more details.
#[macro_export]
macro_rules! flow_end {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        if let Some(context) = $crate::__backend::TraceCategoryContext::acquire($category) {
            $crate::__backend::flow_end(&context, $name, ($flow_id).into(),
                                        &[$($crate::__backend::ArgValue::of($key, $val)),*])
        }
    }
}

/// Constructs a `TraceFutureArgs` object to be passed to `TraceFutureExt::trace`.
///
/// See `fuchsia_trace::trace_future_args!` for more details.
#[macro_export]
macro_rules! trace_future_args {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {{
        let context = $crate::__backend::TraceCategoryContext::acquire($category);
        let args = if context.is_some() {
            vec![$($crate::__backend::ArgValue::of($key, $val)),*]
        } else {
            vec![]
        };
        $crate::__backend::trace_future_args(context, $category, $name, args)
    }}
}

#[cfg(test)]
mod tests {
    use crate::{duration, flow_begin, flow_end, trace_future_args, TraceFutureExt};

    #[fuchsia::test]
    fn test_duration() {
        let trace_only_var = 6;
        duration!(c"category", c"name");
        duration!(c"category", c"name", "arg" => 5);
        duration!(c"category", c"name", "arg" => 5, "arg2" => trace_only_var);
    }

    #[fuchsia::test]
    fn test_flow_begin() {
        let trace_only_var = 6;
        let flow_id = 5u64;
        flow_begin!(c"category", c"name", flow_id);
        flow_begin!(c"category", c"name", flow_id, "arg" => 5);
        flow_begin!(c"category", c"name", flow_id, "arg" => 5, "arg2" => trace_only_var);
    }

    #[fuchsia::test]
    fn test_flow_step() {
        let trace_only_var = 6;
        let flow_id = 5u64;
        flow_step!(c"category", c"name", flow_id);
        flow_step!(c"category", c"name", flow_id, "arg" => 5);
        flow_step!(c"category", c"name", flow_id, "arg" => 5, "arg2" => trace_only_var);
    }

    #[fuchsia::test]
    fn test_flow_end() {
        let trace_only_var = 6;
        let flow_id = 5u64;
        flow_end!(c"category", c"name", flow_id);
        flow_end!(c"category", c"name", flow_id, "arg" => 5);
        flow_end!(c"category", c"name", flow_id, "arg" => 5, "arg2" => trace_only_var);
    }

    #[fuchsia::test]
    async fn test_trace_future() {
        let value = async move { 5 }.trace(trace_future_args!(c"category", c"name")).await;
        assert_eq!(value, 5);

        let value =
            async move { 5 }.trace(trace_future_args!(c"category", c"name", "arg1" => 6)).await;
        assert_eq!(value, 5);

        let trace_only_var = 7;
        let value = async move { 5 }
            .trace(trace_future_args!(c"category", c"name", "arg1" => 6, "ar2" => trace_only_var))
            .await;
        assert_eq!(value, 5);
    }

    #[fuchsia::test]
    fn test_arg_types() {
        duration!(c"category", c"name", "bool" => true);
        duration!(c"category", c"name", "i32" => 5i32, "u32" => 5u32);
        duration!(c"category", c"name", "i64" => 5i64, "u64" => 5u64);
        duration!(c"category", c"name", "isize" => 5isize, "usize" => 5usize);
        duration!(c"category", c"name", "f64" => 5f64);

        let owned_str = "test-str".to_owned();
        duration!(c"category", c"name", "str" => owned_str.as_str());

        let mut value = 5u64;
        duration!(c"category", c"name", "const-ptr" => &value as *const u64);
        duration!(c"category", c"name", "mut-ptr" => &mut value as *mut u64);
    }
}
