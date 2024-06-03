// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// DO NOT EDIT. Generated from FIDL library linux by zither, a Fuchsia platform
// tool.

KERNEL_SYSCALL(channel_write, status_t, /* no attributes */, 6,
               (handle, options, bytes, num_bytes, handles, num_handles),
               (_ZX_SYSCALL_ANNO(use_handle("Fuchsia")) handle_t handle, uint32_t options,
                user_in_ptr<const void> bytes, uint32_t num_bytes,
                _ZX_SYSCALL_ANNO(release_handle("Fuchsia")) user_in_ptr<const handle_t> handles,
                uint32_t num_handles))

KERNEL_SYSCALL(channel_read, status_t, /* no attributes */, 8,
               (handle, options, bytes, handles, num_bytes, num_handles, actual_bytes,
                actual_handles),
               (_ZX_SYSCALL_ANNO(use_handle("FuchsiaUnchecked")) handle_t handle, uint32_t options,
                user_out_ptr<void> bytes,
                _ZX_SYSCALL_ANNO(acquire_handle("FuchsiaUnchecked")) user_out_ptr<handle_t> handles,
                uint32_t num_bytes, uint32_t num_handles, user_out_ptr<uint32_t> actual_bytes,
                user_out_ptr<uint32_t> actual_handles))

VDSO_SYSCALL(clock_get_monotonic, time_t, /* no attributes */, 0, (), (void))

INTERNAL_SYSCALL(clock_get_monotonic_via_kernel, time_t, /* no attributes */, 0, (), (void))

KERNEL_SYSCALL(handle_close_many, status_t, /* no attributes */, 2, (handles, num_handles),
               (_ZX_SYSCALL_ANNO(release_handle("Fuchsia")) user_in_ptr<const handle_t> handles,
                size_t num_handles))

KERNEL_SYSCALL(ktrace_control, status_t, /* no attributes */, 4, (handle, action, options, ptr),
               (_ZX_SYSCALL_ANNO(use_handle("Fuchsia")) handle_t handle, uint32_t action,
                uint32_t options, user_inout_ptr<void> ptr))

BLOCKING_SYSCALL(nanosleep, status_t, /* no attributes */, 1, (deadline), (time_t deadline))

KERNEL_SYSCALL(process_exit, void, __NO_RETURN, 1, (retcode), (int64_t retcode))

VDSO_SYSCALL(system_get_page_size, uint32_t, __CONST, 0, (), (void))

KERNEL_SYSCALL(syscall_next, void, /* no attributes */, 0, (), (void))

KERNEL_SYSCALL(syscall_test0, void, /* no attributes */, 0, (), (void))

KERNEL_SYSCALL(syscall_test1, void, /* no attributes */, 0, (), (void))

KERNEL_SYSCALL(syscall_test2, void, /* no attributes */, 0, (), (void))
