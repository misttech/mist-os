// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/zircon.h>
#include <lib/processargs/processargs.h>
#include <lib/zircon-internal/unique-backtrace.h>
#include <lib/zx/channel.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/syscalls.h>

#include <utility>

#include "bootstrap.h"
#include "diagnostics.h"
#include "zircon.h"

namespace ld {

void TakeLogHandle(StartupData& startup, zx::handle handle) {
  zx_info_handle_basic_t info;
  zx_status_t status = handle.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) [[unlikely]] {
    CRASH_WITH_UNIQUE_BACKTRACE();
  }

  switch (info.type) {
    case ZX_OBJ_TYPE_DEBUGLOG:
      startup.debuglog = zx::debuglog{handle.release()};
      break;
    case ZX_OBJ_TYPE_SOCKET:
      startup.log_socket = zx::socket{handle.release()};
      break;
  }
}

// TODO(fxbug.dev/130483): _start should normally be `[[noreturn]] void` though for the timebeing
// it returns an int for easy testing.
extern "C" int _start(zx_handle_t handle, void* vdso) {
  // First thing, bootstrap our own dynamic linking against ourselves and the
  // vDSO.  For this, nothing should go wrong so use a diagnostics object that
  // crashes the process at the first error.  Before linking against the vDSO
  // is completed successfully, there's no way to make a system call to get an
  // error out anyway.
  auto bootstrap_diag = elfldltl::TrapDiagnostics();
  auto vdso_module = BootstrapVdsoModule(bootstrap_diag, vdso);
  auto self_module = BootstrapSelfModule(bootstrap_diag, vdso_module);
  // Only now can we make the system call to discover the page size.
  const size_t page_size = zx_system_get_page_size();
  CompleteBootstrapModule(vdso_module, page_size);
  CompleteBootstrapModule(self_module, page_size);

  // This will be filled with data from the bootstrap channel.
  StartupData startup;

  // Read the bootstrap processsargs message.
  zx::channel bootstrap{std::exchange(handle, {})};
  uint32_t procargs_nbytes, procargs_nhandles;
  zx_status_t status =
      processargs_message_size(bootstrap.get(), &procargs_nbytes, &procargs_nhandles);
  if (status != ZX_OK) [[unlikely]] {
    CRASH_WITH_UNIQUE_BACKTRACE();
  }
  PROCESSARGS_BUFFER(procargs_buffer, procargs_nbytes);
  zx_handle_t procargs_handles[procargs_nhandles];
  // These will be filled to point into the buffer.
  zx_proc_args_t* procargs;
  uint32_t* procargs_handle_info;
  status = processargs_read(bootstrap.get(), procargs_buffer, procargs_nbytes, procargs_handles,
                            procargs_nhandles, &procargs, &procargs_handle_info);
  if (status != ZX_OK) [[unlikely]] {
    CRASH_WITH_UNIQUE_BACKTRACE();
  }

  for (uint32_t i = 0; i < procargs_nhandles; ++i) {
    // If not otherwise consumed below, the handle will be closed.
    zx::handle handle{std::exchange(procargs_handles[i], {})};
    switch (procargs_handle_info[i]) {
      case PA_HND(PA_FD, STDERR_FILENO):
        TakeLogHandle(startup, std::move(handle));
        break;
    }
  }

  // Now that things are bootstrapped, set up the main diagnostics object.
  auto diag = MakeDiagnostics(startup);

  // Fetch the strings.
  //
  // TODO(mcgrathr): In the real production dynamic linker, the only thing it
  // really needs from any of the strings is just to check the environ strings
  // for "LD_DEBUG=...".  That could be done with a simple search without
  // decoding all the strings.
  char* argv[procargs->args_num + 1];
  char* envp[procargs->environ_num + 1];
  char* names[procargs->names_num + 1];
  status = processargs_strings(procargs_buffer, procargs_nbytes, argv, envp, names);
  if (status != ZX_OK) {
    diag.SystemError("cannot decode processargs strings", elfldltl::ZirconError{status});
  }

  // Bail out before handoff if any errors have been detected.
  CheckErrors(diag);

  cpp20::span args{argv, procargs->args_num};
  int result = 0;
  for (std::string_view str : args) {
    diag.report()(str);
    result += static_cast<int>(str.size());
  }

  return result;
}

void ReportError(StartupData& startup, std::string_view str) {
  // If we have a debuglog handle, use that.
  if (startup.debuglog) {
    startup.debuglog.write(0, str.data(), str.size());
  }

  // We might instead (or also?) have a socket, where the messages are easier
  // to capture at the other end.
  if (startup.log_socket) {
    while (true) {
      size_t wrote = 0;
      zx_status_t status = startup.log_socket.write(0, str.data(), str.size(), &wrote);
      if (status != ZX_OK) {
        break;
      }
      str.remove_prefix(wrote);
      if (str.empty()) {
        startup.log_socket.write(0, "\n", 1, nullptr);
        break;
      }
    }
  }
}

}  // namespace ld
