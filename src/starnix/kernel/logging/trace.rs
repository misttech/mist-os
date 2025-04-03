// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use fuchsia_trace::Scope as TraceScope;

// This needs to be available to the macros in this module without clients having to depend on
// fuchsia_trace themselves.
#[doc(hidden)]
pub use fuchsia_trace as __fuchsia_trace;

use std::ffi::CStr;

// The trace category used for starnix-related traces.
pub const CATEGORY_STARNIX: &'static CStr = c"starnix";

// The trace category used for memory manager related traces.
pub const CATEGORY_STARNIX_MM: &'static CStr = c"starnix:mm";

// The trace category used for security related traces.
pub const CATEGORY_STARNIX_SECURITY: &'static CStr = c"starnix:security";

// The name used to track the duration in Starnix while executing a task.
pub const NAME_RUN_TASK: &'static CStr = c"RunTask";

// The trace category used for atrace events generated within starnix.
pub const CATEGORY_ATRACE: &'static CStr = c"starnix:atrace";

// The name used to identify blob records from the container's Perfetto daemon.
pub const NAME_PERFETTO_BLOB: &'static CStr = c"starnix_perfetto";

// The name used to track the duration of a syscall.
pub const NAME_EXECUTE_SYSCALL: &'static CStr = c"ExecuteSyscall";

// The name used to track the duration of creating a container.
pub const NAME_CREATE_CONTAINER: &'static CStr = c"CreateContainer";

// The name used to track the start time of the starnix kernel.
pub const NAME_START_KERNEL: &'static CStr = c"StartKernel";

// The name used to track when a thread was kicked.
pub const NAME_RESTRICTED_KICK: &'static CStr = c"RestrictedKick";

// The name used to track the duration for inline exception handling.
pub const NAME_HANDLE_EXCEPTION: &'static CStr = c"HandleException";

// The names used to track durations for restricted state I/O.
pub const NAME_READ_RESTRICTED_STATE: &'static CStr = c"ReadRestrictedState";
pub const NAME_WRITE_RESTRICTED_STATE: &'static CStr = c"WriteRestrictedState";

// The name used to track the duration of checking whether the task loop should exit.
pub const NAME_CHECK_TASK_EXIT: &'static CStr = c"CheckTaskExit";

pub const ARG_NAME: &'static str = "name";

#[inline]
pub const fn regular_tracing_enabled() -> bool {
    cfg!(feature = "tracing")
}

#[inline]
pub fn regular_trace_category_enabled(category: &'static CStr) -> bool {
    regular_tracing_enabled() && fuchsia_trace::category_enabled(category)
}

#[inline]
pub const fn firehose_tracing_enabled() -> bool {
    cfg!(feature = "tracing_firehose")
}

#[macro_export]
macro_rules! trace_instant {
    ($category:expr, $name:expr, $scope:expr $(, $key:expr => $val:expr)*) => {
        if $crate::regular_tracing_enabled() {
            $crate::__fuchsia_trace::instant!($category, $name, $scope $(, $key => $val)*);
        }
    };
}

#[macro_export]
macro_rules! firehose_trace_instant {
    ($category:expr, $name:expr, $scope:expr $(, $key:expr => $val:expr)*) => {
        if $crate::firehose_tracing_enabled() {
            $crate::trace_instant!($category, $name, $scope $(, $key => $val)*);
        }
    }
}

// The `trace_duration` macro defines a `_scope` instead of executing a statement because the
// lifetime of the `_scope` variable corresponds to the duration.
#[macro_export]
macro_rules! trace_duration {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        let _args = if $crate::regular_tracing_enabled() {
            Some([$($crate::__fuchsia_trace::ArgValue::of($key, $val)),*])
        } else {
            None
        };
        let _scope = if $crate::regular_tracing_enabled() {
            Some($crate::__fuchsia_trace::duration(
                $category,
                $name,
                _args.as_ref().unwrap()
            ))
        } else {
            None
        };
    }
}

#[macro_export]
macro_rules! firehose_trace_duration {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        if $crate::firehose_tracing_enabled() {
            $crate::trace_duration!($category, $name $(, $key => $val)*);
        }
    }
}

#[macro_export]
macro_rules! trace_duration_begin {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        if $crate::regular_tracing_enabled() {
            $crate::__fuchsia_trace::duration_begin!($category, $name $(, $key => $val)*);
        }
    };
}

#[macro_export]
macro_rules! firehose_trace_duration_begin {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        if $crate::firehose_tracing_enabled() {
            $crate::trace_duration_begin!($category, $name $(, $key => $val)*);
        }
    }
}

#[macro_export]
macro_rules! trace_duration_end {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        if $crate::regular_tracing_enabled() {
            $crate::__fuchsia_trace::duration_end!($category, $name $(, $key => $val)*);
        }
    };
}

#[macro_export]
macro_rules! firehose_trace_duration_end {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        if $crate::firehose_tracing_enabled() {
            $crate::trace_duration_end!($category, $name $(, $key => $val)*);
        }
    }
}

#[macro_export]
macro_rules! trace_flow_begin {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        let _flow_id: $crate::__fuchsia_trace::Id = $flow_id;
        if $crate::regular_tracing_enabled() {
            $crate::__fuchsia_trace::flow_begin!($category, $name, _flow_id $(, $key => $val)*);
        }
    };
}

#[macro_export]
macro_rules! trace_flow_step {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        let _flow_id: $crate::__fuchsia_trace::Id = $flow_id;
        if $crate::regular_tracing_enabled() {
            $crate::__fuchsia_trace::flow_step!($category, $name, _flow_id $(, $key => $val)*);
        }
    };
}

#[macro_export]
macro_rules! trace_flow_end {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        let _flow_id: $crate::__fuchsia_trace::Id = $flow_id;
        if $crate::regular_tracing_enabled() {
            $crate::__fuchsia_trace::flow_end!($category, $name, _flow_id $(, $key => $val)*);
        }
    };
}

#[macro_export]
macro_rules! trace_instant_flow_begin {
    (
        $category:expr,
        $event_name:expr,
        $flow_name:expr,
        $flow_id:expr
        $(, $key:expr => $val:expr)*
    ) => {
        let _flow_id: $crate::__fuchsia_trace::Id = $flow_id;
        if $crate::regular_tracing_enabled() {
            $crate::__fuchsia_trace::instant_flow_begin!(
                $category,
                $event_name,
                $flow_name,
                _flow_id
                $(, $key => $val)*
            );
        }
    };
}

#[macro_export]
macro_rules! trace_instant_flow_end {
    (
        $category:expr,
        $event_name:expr,
        $flow_name:expr,
        $flow_id:expr
        $(, $key:expr => $val:expr)*
    ) => {
        let _flow_id: $crate::__fuchsia_trace::Id = $flow_id;
        if $crate::regular_tracing_enabled() {
            $crate::__fuchsia_trace::instant_flow_end!(
                $category,
                $event_name,
                $flow_name,
                _flow_id
                $(, $key => $val)*
            );
        }
    };
}

#[macro_export]
macro_rules! trace_instant_flow_step {
    (
        $category:expr,
        $event_name:expr,
        $flow_name:expr,
        $flow_id:expr
        $(, $key:expr => $val:expr)*
    ) => {
        let _flow_id: $crate::__fuchsia_trace::Id = $flow_id;
        if $crate::regular_tracing_enabled() {
            $crate::__fuchsia_trace::instant_flow_step!(
                $category,
                $event_name,
                $flow_name,
                _flow_id
                $(, $key => $val)*
            );
        }
    };
}
