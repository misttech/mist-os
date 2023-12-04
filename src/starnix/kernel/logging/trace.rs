// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// The trace category used for starnix-related traces.
fuchsia_trace::string_name_macro!(trace_category_starnix, "starnix");

// The trace category used for memory manager related traces.
fuchsia_trace::string_name_macro!(trace_category_starnix_mm, "starnix:mm");

// The trace category used to enable task runtime args on trace durations.
fuchsia_trace::string_name_macro!(trace_category_starnix_task_runtime, "starnix:task_runtime");

// The name used to track the duration in Starnix while executing a task.
fuchsia_trace::string_name_macro!(trace_name_run_task, "RunTask");

// The trace category used for atrace events generated within starnix.
fuchsia_trace::string_name_macro!(trace_category_atrace, "starnix:atrace");

// The name used to identify blob records from the container's Perfetto daemon.
fuchsia_trace::string_name_macro!(trace_name_perfetto_blob, "starnix_perfetto");

// The name used to track the duration in Starnix while executing a syscall.
fuchsia_trace::string_name_macro!(trace_name_normal_mode, "NormalMode");

// The name used to track the duration in user space between syscalls.
fuchsia_trace::string_name_macro!(trace_name_restricted_mode, "RestrictedMode");

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

// The argument used to track the CPU time of a thread at various points during a syscall.
fuchsia_trace::string_name_macro!(trace_arg_cpu_time, "cpu_time");

// The argument used to track the queue time of a thread at various points during a syscall.
fuchsia_trace::string_name_macro!(trace_arg_queue_time, "queue_time");

// The argument used to track the page fault time of a thread at various points during a syscall.
fuchsia_trace::string_name_macro!(trace_arg_page_fault_time, "page_fault_time");

// The argument used to track the lock contention time of a thread at various points during a
// syscall.
fuchsia_trace::string_name_macro!(trace_arg_lock_contention_time, "lock_contention_time");

#[macro_export]
macro_rules! ignore_unused_variables_if_disable_tracing {
    ($($val:expr),*) => {
        $(
            #[cfg(not(feature = "tracing"))]
            { let _ = &$val; }
        )*
    };
}

#[macro_export]
macro_rules! trace_instant {
    ($category:expr, $name:expr, $scope:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(feature = "tracing")]
        fuchsia_trace::instant!($category, $name, $scope $(, $key => $val)*);

        $crate::ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

// The `trace_duration` macro defines a `_scope` instead of executing a statement because the
// lifetime of the `_scope` variable corresponds to the duration.
#[macro_export]
macro_rules! trace_duration {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(feature = "tracing")]
        let _args = [$(fuchsia_trace::ArgValue::of($key, $val)),*];
        #[cfg(feature = "tracing")]
        let _scope = fuchsia_trace::duration(fuchsia_trace::cstr!($category), fuchsia_trace::cstr!($name), &_args);

        $crate::ignore_unused_variables_if_disable_tracing!($($val),*);
    }
}

#[macro_export]
macro_rules! trace_duration_begin {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(feature = "tracing")]
        fuchsia_trace::duration_begin!($category, $name $(, $key => $val)*);

        $crate::ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

#[macro_export]
macro_rules! trace_duration_end {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(feature = "tracing")]
        fuchsia_trace::duration_end!($category, $name $(, $key => $val)*);

        $crate::ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

#[macro_export]
macro_rules! trace_flow_begin {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        let _flow_id: fuchsia_trace::Id = $flow_id;

        #[cfg(feature = "tracing")]
        fuchsia_trace::flow_begin!($category, $name, _flow_id $(, $key => $val)*);

        $crate::ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

#[macro_export]
macro_rules! trace_flow_step {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        let _flow_id: fuchsia_trace::Id = $flow_id;

        #[cfg(feature = "tracing")]
        fuchsia_trace::flow_step!($category, $name, _flow_id $(, $key => $val)*);

        $crate::ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

#[macro_export]
macro_rules! trace_flow_end {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        let _flow_id: fuchsia_trace::Id = $flow_id;

        #[cfg(feature = "tracing")]
        fuchsia_trace::flow_end!($category, $name, _flow_id $(, $key => $val)*);

        $crate::ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}
