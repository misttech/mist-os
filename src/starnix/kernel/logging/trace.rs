// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use fuchsia_trace::Scope as TraceScope;

// This needs to be available to the macros in this module without clients having to depend on
// fuchsia_trace themselves.
#[doc(hidden)]
pub use fuchsia_trace as __fuchsia_trace;

// The trace category used for starnix-related traces.
fuchsia_trace::string_name_macro!(trace_category_starnix, "starnix");

// The trace category used for memory manager related traces.
fuchsia_trace::string_name_macro!(trace_category_starnix_mm, "starnix:mm");

// The name used to track the duration in Starnix while executing a task.
fuchsia_trace::string_name_macro!(trace_name_run_task, "RunTask");

// The trace category used for atrace events generated within starnix.
fuchsia_trace::string_name_macro!(trace_category_atrace, "starnix:atrace");

// The name used to identify blob records from the container's Perfetto daemon.
fuchsia_trace::string_name_macro!(trace_name_perfetto_blob, "starnix_perfetto");

// The name used to track the duration of a syscall.
fuchsia_trace::string_name_macro!(trace_name_execute_syscall, "ExecuteSyscall");

// The name used to track the duration of creating a container.
fuchsia_trace::string_name_macro!(trace_name_create_container, "CreateContainer");

// The name used to track the start time of the starnix kernel.
fuchsia_trace::string_name_macro!(trace_name_start_kernel, "StartKernel");

// The name used to track when a thread was kicked.
fuchsia_trace::string_name_macro!(trace_name_restricted_kick, "RestrictedKick");

// The name used to track the duration for inline exception handling.
fuchsia_trace::string_name_macro!(trace_name_handle_exception, "HandleException");

// The names used to track durations for restricted state I/O.
fuchsia_trace::string_name_macro!(trace_name_read_restricted_state, "ReadRestrictedState");
fuchsia_trace::string_name_macro!(trace_name_write_restricted_state, "WriteRestrictedState");

// The name used to track the duration of checking whether the task loop should exit.
fuchsia_trace::string_name_macro!(trace_name_check_task_exit, "CheckTaskExit");

// The argument used to track the name of a syscall.
fuchsia_trace::string_name_macro!(trace_arg_name, "name");

#[inline]
pub const fn regular_tracing_enabled() -> bool {
    cfg!(feature = "tracing")
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
                $crate::__fuchsia_trace::cstr!($category),
                $crate::__fuchsia_trace::cstr!($name),
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
