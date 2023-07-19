// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/zircon.h>
#include <lib/llvm-profdata/llvm-profdata.h>
#include <lib/processargs/processargs.h>
#include <lib/trivial-allocator/new.h>
#include <lib/trivial-allocator/zircon.h>
#include <lib/zircon-internal/unique-backtrace.h>
#include <lib/zx/channel.h>
#include <lib/zx/eventpair.h>
#include <lib/zx/vmo.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/syscalls.h>

#include <optional>
#include <string_view>
#include <utility>

#include "allocator.h"
#include "bootstrap.h"
#include "diagnostics.h"
#include "zircon.h"

namespace ld {
namespace {

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

// TODO(fxbug.dev/130542): After LlvmProfdata:UseCounters, functions will load
// the new value of __llvm_profile_counter_bias and use it. However, functions
// already in progress will use a cached value from before it changed. This
// means they'll still be pointing into the data segment and updating the old
// counters there. So they'd crash with write faults if it were protected.
// There may be a way to work around this by having uninstrumented functions
// call instrumented functions such that the tail return path of any frame live
// across the transition is uninstrumented. Note that each function will
// resample even if that function is inlined into a caller that itself will
// still be using the stale pointer. However, in the long run we expect to move
// from the relocatable-counters design to a new design where the counters are
// in a separate "bss-like" location that we arrange to be in a separate VMO
// created by the program loader. If we do that, then this issue won't arise,
// so we might not get around to making protecting the data compatible with
// profdata instrumentation before it's moot.
constexpr bool kProtectData = !HAVE_LLVM_PROFDATA;

zx::eventpair PublishProfdata(Diagnostics& diag, zx::unowned_vmar vmar,
                              cpp20::span<const std::byte> build_id) {
#if HAVE_LLVM_PROFDATA
  auto error = [&diag](zx_status_t status, auto&&... args) -> zx::eventpair {
    diag.SystemError(std::forward<decltype(args)>(args)..., elfldltl::ZirconError{status});
    return {};
  };

  LlvmProfdata profdata;
  profdata.Init(build_id);
  const size_t size = profdata.size_bytes();
  if (size != 0) {
    // Make a VMO and map it in to hold the profdata.
    zx::vmo vmo;
    zx_status_t status = zx::vmo::create(size, 0, &vmo);
    if (status != ZX_OK) {
      return error(status, "cannot create llvm-profdata VMO of ", size, " bytes");
    }
    uintptr_t ptr;
    status = vmar->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, size, &ptr);
    if (status != ZX_OK) {
      return error(status, "cannot map llvm-profdata VMO of ", size, " bytes");
    }
    cpp20::span vmo_data{reinterpret_cast<std::byte*>(ptr), size};

    // Now fill the VMO and redirect the instrumentation to update its data.
    cpp20::span counters = profdata.WriteFixedData(vmo_data);
    profdata.CopyCounters(counters);
    LlvmProfdata::UseCounters(counters);

    // At this point the instrumentation will no longer touch the data segment.

    zx::eventpair local_token, remote_token;
    status = zx::eventpair::create(0, &local_token, &remote_token);
    if (status != ZX_OK) {
      return error(status, "zx_eventpair_create");
    }

    // TODO(fxbug.dev/130542): Send the VMO and remote_token in a
    // fuchsia.debugdata.Publisher/Publish message to ... somewhere.

    return local_token;
  }
#endif
  return {};
}

}  // namespace

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

  zx::vmar loading_vmar, self_vmar;
  for (uint32_t i = 0; i < procargs_nhandles; ++i) {
    // If not otherwise consumed below, the handle will be closed.
    zx::handle handle{std::exchange(procargs_handles[i], {})};
    switch (procargs_handle_info[i]) {
      case PA_VMAR_ROOT:
        loading_vmar.reset(handle.release());
        break;

      case PA_VMAR_LOADED:
        self_vmar.reset(handle.release());
        break;

      case PA_HND(PA_FD, STDERR_FILENO):
        TakeLogHandle(startup, std::move(handle));
        break;
    }
  }

  // Now that things are bootstrapped, set up the main diagnostics object.
  auto diag = MakeDiagnostics(startup);

  // Set up the allocators.  These objects hold zx::unowned_vmar copies but do
  // not own the VMAR handle.
  trivial_allocator::ZirconVmar system_page_allocator{loading_vmar};
  auto scratch = MakeScratchAllocator(system_page_allocator);
  auto initial_exec = MakeInitialExecAllocator(system_page_allocator);

  auto alloc_check = [&diag](fbl::AllocChecker& ac, std::string_view what,
                             std::optional<size_t> count = std::nullopt) {
    if (ac.check()) [[likely]] {
      return;
    }
    if (count) {
      diag.SystemError("cannot allocate", count, what, elfldltl::ZirconError{ZX_ERR_NO_MEMORY});
    } else {
      diag.SystemError("cannot allocate", what, elfldltl::ZirconError{ZX_ERR_NO_MEMORY});
    }
    __builtin_trap();
  };

  // Fetch the strings.
  //
  // TODO(mcgrathr): In the real production dynamic linker, the only thing it
  // really needs from any of the strings is just to check the environ strings
  // for "LD_DEBUG=...".  That could be done with a simple search without
  // decoding all the strings.
  auto make_string_array = [&scratch, &alloc_check](size_t count, std::string_view what) -> char** {
    fbl::AllocChecker ac;
    char** strings = new (scratch, ac) char*[count];
    alloc_check(ac, what, count);
    return strings;
  };
  char** argv = make_string_array(procargs->args_num + 1, "argument stringpointers");
  char** envp = make_string_array(procargs->environ_num + 1, "environment string pointers");
  char** names = make_string_array(procargs->names_num + 1, "name table string pointers");
  status = processargs_strings(procargs_buffer, procargs_nbytes, argv, envp, names);
  if (status != ZX_OK) {
    diag.SystemError("cannot decode processargs strings", elfldltl::ZirconError{status});
  }

  // Start publishing profiling data in an instrumented build.  Before this,
  // the instrumentation is updating counters in the data segment.  After this,
  // it's updating a VMO mapped elsewhere.  That VMO remains mapped after
  // startup just to avoid bothering with code to unmap it since that code and
  // the rest of the return path would have to be uninstrumented.  When the
  // returned handle is closed by going out of scope at the end of startup,
  // this will signal the data receiver that the VMO's data is ready.  It's
  // still possible for either the last bit of instrumented code in the startup
  // path, or just stray pointer writes in the process after startup will
  // modify it, but will be ignored or will be tolerable noise in the data.
  auto profdata = PublishProfdata(diag, loading_vmar.borrow(), self_module.build_id);

  // Now that startup is completed, protect not only the RELRO, but also all
  // the data and bss.  Then drop that VMAR handle so the protections cannot be
  // changed again.
  auto protect_data = [page_size, &diag](zx::vmar self) {
    auto [data_start, data_size] = DataBounds(page_size);
    zx_status_t status = self.protect(ZX_VM_PERM_READ, data_start, data_size);
    if (status != ZX_OK) [[unlikely]] {
      diag.SystemError("cannot protect dynamic linker data pages", elfldltl::ZirconError{status});
    }
  };
  if constexpr (kProtectData) {
    protect_data(std::move(self_vmar));
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
