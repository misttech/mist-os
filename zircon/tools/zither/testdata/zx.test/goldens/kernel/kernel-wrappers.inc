// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// DO NOT EDIT. Generated from FIDL library zx by zither, a Fuchsia platform
// tool.

extern "C" {

syscall_result wrapper_channel_read(SafeSyscallArgument<zx_handle_t>::RawType handle,
                                    SafeSyscallArgument<uint32_t>::RawType options,
                                    SafeSyscallArgument<void*>::RawType bytes,
                                    SafeSyscallArgument<zx_handle_t*>::RawType handles,
                                    SafeSyscallArgument<uint32_t>::RawType num_bytes,
                                    SafeSyscallArgument<uint32_t>::RawType num_handles,
                                    SafeSyscallArgument<uint32_t*>::RawType actual_bytes,
                                    SafeSyscallArgument<uint32_t*>::RawType actual_handles,
                                    uint64_t pc);
syscall_result wrapper_channel_read(SafeSyscallArgument<zx_handle_t>::RawType handle,
                                    SafeSyscallArgument<uint32_t>::RawType options,
                                    SafeSyscallArgument<void*>::RawType bytes,
                                    SafeSyscallArgument<zx_handle_t*>::RawType handles,
                                    SafeSyscallArgument<uint32_t>::RawType num_bytes,
                                    SafeSyscallArgument<uint32_t>::RawType num_handles,
                                    SafeSyscallArgument<uint32_t*>::RawType actual_bytes,
                                    SafeSyscallArgument<uint32_t*>::RawType actual_handles,
                                    uint64_t pc) {
  return do_syscall(
      ZX_SYS_channel_read, pc, &VDso::ValidSyscallPC::channel_read,
      [&](ProcessDispatcher* current_process) -> uint64_t {
        auto result = sys_channel_read(
            SafeSyscallArgument<zx_handle_t>::Sanitize(handle),
            SafeSyscallArgument<uint32_t>::Sanitize(options),
            make_user_out_ptr(SafeSyscallArgument<void*>::Sanitize(bytes)),
            make_user_out_ptr(SafeSyscallArgument<zx_handle_t*>::Sanitize(handles)),
            SafeSyscallArgument<uint32_t>::Sanitize(num_bytes),
            SafeSyscallArgument<uint32_t>::Sanitize(num_handles),
            make_user_out_ptr(SafeSyscallArgument<uint32_t*>::Sanitize(actual_bytes)),
            make_user_out_ptr(SafeSyscallArgument<uint32_t*>::Sanitize(actual_handles)));
        return result;
      });
}

syscall_result wrapper_channel_write(SafeSyscallArgument<zx_handle_t>::RawType handle,
                                     SafeSyscallArgument<uint32_t>::RawType options,
                                     SafeSyscallArgument<const void*>::RawType bytes,
                                     SafeSyscallArgument<uint32_t>::RawType num_bytes,
                                     SafeSyscallArgument<const zx_handle_t*>::RawType handles,
                                     SafeSyscallArgument<uint32_t>::RawType num_handles,
                                     uint64_t pc);
syscall_result wrapper_channel_write(SafeSyscallArgument<zx_handle_t>::RawType handle,
                                     SafeSyscallArgument<uint32_t>::RawType options,
                                     SafeSyscallArgument<const void*>::RawType bytes,
                                     SafeSyscallArgument<uint32_t>::RawType num_bytes,
                                     SafeSyscallArgument<const zx_handle_t*>::RawType handles,
                                     SafeSyscallArgument<uint32_t>::RawType num_handles,
                                     uint64_t pc) {
  return do_syscall(
      ZX_SYS_channel_write, pc, &VDso::ValidSyscallPC::channel_write,
      [&](ProcessDispatcher* current_process) -> uint64_t {
        auto result = sys_channel_write(
            SafeSyscallArgument<zx_handle_t>::Sanitize(handle),
            SafeSyscallArgument<uint32_t>::Sanitize(options),
            make_user_in_ptr(SafeSyscallArgument<const void*>::Sanitize(bytes)),
            SafeSyscallArgument<uint32_t>::Sanitize(num_bytes),
            make_user_in_ptr(SafeSyscallArgument<const zx_handle_t*>::Sanitize(handles)),
            SafeSyscallArgument<uint32_t>::Sanitize(num_handles));
        return result;
      });
}

syscall_result wrapper_clock_get_monotonic_via_kernel(uint64_t pc);
syscall_result wrapper_clock_get_monotonic_via_kernel(uint64_t pc) {
  return do_syscall(ZX_SYS_clock_get_monotonic_via_kernel, pc,
                    &VDso::ValidSyscallPC::clock_get_monotonic_via_kernel,
                    [&](ProcessDispatcher* current_process) -> uint64_t {
                      auto result = sys_clock_get_monotonic_via_kernel();
                      return result;
                    });
}

syscall_result wrapper_handle_close_many(SafeSyscallArgument<const zx_handle_t*>::RawType handles,
                                         SafeSyscallArgument<size_t>::RawType num_handles,
                                         uint64_t pc);
syscall_result wrapper_handle_close_many(SafeSyscallArgument<const zx_handle_t*>::RawType handles,
                                         SafeSyscallArgument<size_t>::RawType num_handles,
                                         uint64_t pc) {
  return do_syscall(
      ZX_SYS_handle_close_many, pc, &VDso::ValidSyscallPC::handle_close_many,
      [&](ProcessDispatcher* current_process) -> uint64_t {
        auto result = sys_handle_close_many(
            make_user_in_ptr(SafeSyscallArgument<const zx_handle_t*>::Sanitize(handles)),
            SafeSyscallArgument<size_t>::Sanitize(num_handles));
        return result;
      });
}

syscall_result wrapper_ktrace_control(SafeSyscallArgument<zx_handle_t>::RawType handle,
                                      SafeSyscallArgument<uint32_t>::RawType action,
                                      SafeSyscallArgument<uint32_t>::RawType options,
                                      SafeSyscallArgument<void*>::RawType ptr, uint64_t pc);
syscall_result wrapper_ktrace_control(SafeSyscallArgument<zx_handle_t>::RawType handle,
                                      SafeSyscallArgument<uint32_t>::RawType action,
                                      SafeSyscallArgument<uint32_t>::RawType options,
                                      SafeSyscallArgument<void*>::RawType ptr, uint64_t pc) {
  return do_syscall(ZX_SYS_ktrace_control, pc, &VDso::ValidSyscallPC::ktrace_control,
                    [&](ProcessDispatcher* current_process) -> uint64_t {
                      auto result = sys_ktrace_control(
                          SafeSyscallArgument<zx_handle_t>::Sanitize(handle),
                          SafeSyscallArgument<uint32_t>::Sanitize(action),
                          SafeSyscallArgument<uint32_t>::Sanitize(options),
                          make_user_inout_ptr(SafeSyscallArgument<void*>::Sanitize(ptr)));
                      return result;
                    });
}

syscall_result wrapper_nanosleep(SafeSyscallArgument<zx_time_t>::RawType deadline, uint64_t pc);
syscall_result wrapper_nanosleep(SafeSyscallArgument<zx_time_t>::RawType deadline, uint64_t pc) {
  return do_syscall(ZX_SYS_nanosleep, pc, &VDso::ValidSyscallPC::nanosleep,
                    [&](ProcessDispatcher* current_process) -> uint64_t {
                      auto result =
                          sys_nanosleep(SafeSyscallArgument<zx_time_t>::Sanitize(deadline));
                      return result;
                    });
}

syscall_result wrapper_process_exit(SafeSyscallArgument<int64_t>::RawType retcode, uint64_t pc);
syscall_result wrapper_process_exit(SafeSyscallArgument<int64_t>::RawType retcode, uint64_t pc) {
  return do_syscall(
      ZX_SYS_process_exit, pc, &VDso::ValidSyscallPC::process_exit,
      [&](ProcessDispatcher* current_process) -> uint64_t {
        /*noreturn*/ sys_process_exit(SafeSyscallArgument<int64_t>::Sanitize(retcode));
        /* NOTREACHED */
        return ZX_ERR_BAD_STATE;
      });
}

syscall_result wrapper_syscall_next(uint64_t pc);
syscall_result wrapper_syscall_next(uint64_t pc) {
  return do_syscall(ZX_SYS_syscall_next, pc, &VDso::ValidSyscallPC::syscall_next,
                    [&](ProcessDispatcher* current_process) -> uint64_t {
                      auto result = sys_syscall_next();
                      return result;
                    });
}

syscall_result wrapper_syscall_test0(uint64_t pc);
syscall_result wrapper_syscall_test0(uint64_t pc) {
  return do_syscall(ZX_SYS_syscall_test0, pc, &VDso::ValidSyscallPC::syscall_test0,
                    [&](ProcessDispatcher* current_process) -> uint64_t {
                      auto result = sys_syscall_test0();
                      return result;
                    });
}

syscall_result wrapper_syscall_test1(uint64_t pc);
syscall_result wrapper_syscall_test1(uint64_t pc) {
  return do_syscall(ZX_SYS_syscall_test1, pc, &VDso::ValidSyscallPC::syscall_test1,
                    [&](ProcessDispatcher* current_process) -> uint64_t {
                      auto result = sys_syscall_test1();
                      return result;
                    });
}

syscall_result wrapper_syscall_test2(uint64_t pc);
syscall_result wrapper_syscall_test2(uint64_t pc) {
  return do_syscall(ZX_SYS_syscall_test2, pc, &VDso::ValidSyscallPC::syscall_test2,
                    [&](ProcessDispatcher* current_process) -> uint64_t {
                      auto result = sys_syscall_test2();
                      return result;
                    });
}
}
