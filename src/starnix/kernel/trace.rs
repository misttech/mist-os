// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// The trace category used for starnix-related traces.
fuchsia_trace::string_name_macro!(trace_category_starnix, "starnix");

// The name used to track the duration in Starnix while executing a syscall.
fuchsia_trace::string_name_macro!(trace_name_run_task_loop, "RunTaskLoop");

// The name used to track the duration in user space between syscalls.
fuchsia_trace::string_name_macro!(trace_name_user_space, "UserSpace");

// The name used to track the duration of a syscall.
fuchsia_trace::string_name_macro!(trace_name_execute_syscall, "ExecuteSyscall");

// The name used to track the duration of creating a container.
fuchsia_trace::string_name_macro!(trace_name_create_container, "CreateContainer");

// The name used to track the start time of the starnix kernel.
fuchsia_trace::string_name_macro!(trace_name_start_kernel, "StartKernel");

// The argument used to track the name of a syscall.
fuchsia_trace::string_name_macro!(trace_arg_name, "name");

macro_rules! ignore_unused_variables_if_disable_tracing {
    ($($val:expr),*) => {
        $(
            #[cfg(feature = "disable_tracing")]
            { let _ = &$val; }
        )*
    };
}

macro_rules! trace_instant {
    ($category:expr, $name:expr, $scope:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(not(feature = "disable_tracing"))]
        fuchsia_trace::instant!($category, $name, $scope $(, $key => $val)*);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

// The `trace_duration` macro defines a `_scope` instead of executing a statement because the
// lifetime of the `_scope` variable corresponds to the duration.
macro_rules! trace_duration {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(not(feature = "disable_tracing"))]
        let _args = [$(fuchsia_trace::ArgValue::of($key, $val)),*];
        #[cfg(not(feature = "disable_tracing"))]
        let _scope = fuchsia_trace::duration(fuchsia_trace::cstr!($category), fuchsia_trace::cstr!($name), &_args);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    }
}

macro_rules! trace_duration_begin {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(not(feature = "disable_tracing"))]
        fuchsia_trace::duration_begin!($category, $name $(, $key => $val)*);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

macro_rules! trace_duration_end {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(not(feature = "disable_tracing"))]
        fuchsia_trace::duration_end!($category, $name $(, $key => $val)*);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

macro_rules! trace_flow_begin {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        let _flow_id: fuchsia_trace::Id = $flow_id;

        #[cfg(not(feature = "disable_tracing"))]
        fuchsia_trace::flow_begin!($category, $name, _flow_id $(, $key => $val)*);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

macro_rules! trace_flow_step {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        let _flow_id: fuchsia_trace::Id = $flow_id;

        #[cfg(not(feature = "disable_tracing"))]
        fuchsia_trace::flow_step!($category, $name, _flow_id $(, $key => $val)*);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

macro_rules! trace_flow_end {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        let _flow_id: fuchsia_trace::Id = $flow_id;

        #[cfg(not(feature = "disable_tracing"))]
        fuchsia_trace::flow_end!($category, $name, _flow_id $(, $key => $val)*);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}
