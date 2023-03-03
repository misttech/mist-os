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
