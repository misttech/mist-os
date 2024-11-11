// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/vmo.h>
#include <lib/elfldltl/zircon.h>
#include <lib/ld/fuchsia-debugdata.h>
#include <lib/llvm-profdata/llvm-profdata.h>
#include <lib/trivial-allocator/new.h>
#include <lib/trivial-allocator/zircon.h>
#include <lib/zx/channel.h>
#include <lib/zx/eventpair.h>
#include <lib/zx/vmo.h>
#include <zircon/compiler.h>
#include <zircon/syscalls.h>

#include <optional>
#include <string_view>
#include <utility>

#include "allocator.h"
#include "bootstrap.h"
#include "startup-diagnostics.h"
#include "zircon.h"

namespace ld {
namespace {

using VmoFile = elfldltl::VmoFile<Diagnostics>;

using SystemPageAllocator = trivial_allocator::ZirconVmar;

auto MakeStartupSystemPageAllocator(StartupData& startup) {
  return SystemPageAllocator{startup.vmar};
}

auto MakeStartupScratchAllocator(SystemPageAllocator system) {
  return MakeScratchAllocator(system);
}

using ScratchAllocator = decltype(MakeStartupScratchAllocator(SystemPageAllocator{}));

auto MakeStartupInitialExecAllocator(SystemPageAllocator system) {
  return MakeInitialExecAllocator(system);
}

using InitialExecAllocator = decltype(MakeStartupInitialExecAllocator(SystemPageAllocator{}));

struct LoadExecutableResult : public StartupLoadResult {
  StartupModule* module = nullptr;
};

LoadExecutableResult LoadExecutable(Diagnostics& diag, StartupData& startup,
                                    ScratchAllocator& scratch, InitialExecAllocator& initial_exec,
                                    zx::vmo vmo) {
  LoadExecutableResult result = {
      .module = StartupModule::New(diag, scratch, abi::Abi<>::kExecutableName, startup.vmar),
  };
  if (!vmo) [[unlikely]] {
    diag.SystemError("no executable VMO in bootstrap message");
  } else {
    elfldltl::UnownedVmoFile file{vmo.borrow(), diag};
    Elf::size_type max_tls_modid = 0;
    static_cast<StartupLoadResult&>(result) =
        result.module->Load(diag, initial_exec, file, 0, max_tls_modid);
    assert(max_tls_modid <= 1);
  }
  return result;
}

[[maybe_unused]] void ProtectData(Diagnostics& diag, size_t page_size, zx::vmar self) {
  auto [data_start, data_size] = DataBounds(page_size);
  zx_status_t status = self.protect(ZX_VM_PERM_READ, data_start, data_size);
  if (status != ZX_OK) [[unlikely]] {
    diag.SystemError("cannot protect dynamic linker data pages", elfldltl::ZirconError{status});
  }
}

ld::Debugdata::Deferred PublishProfdata(Diagnostics& diag, zx::unowned_vmar vmar,
                                        std::span<const std::byte> build_id) {
#if HAVE_LLVM_PROFDATA
  auto error = [&diag](zx_status_t status, auto&&... args) -> ld::Debugdata::Deferred {
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
    std::span vmo_data{reinterpret_cast<std::byte*>(ptr), size};

    // Now fill the VMO and redirect the instrumentation to update its data.
    auto live_data = profdata.WriteFixedData(vmo_data);
    profdata.CopyLiveData(live_data);
    LlvmProfdata::UseLiveData(live_data);

    // At this point the instrumentation will no longer touch the data segment.
    ld::Debugdata debugdata{LlvmProfdata::kDataSinkName, std::move(vmo)};
    zx::result deferred = std::move(debugdata).DeferredPublish();
    if (deferred.is_error()) {
      return error(status, "cannot publish ", LlvmProfdata::kDataSinkName, " to fuchsia.debugdata");
    }

    return *std::move(deferred);
  }
#endif
  return {};
}

}  // namespace

// The _start assembly code saves the two argument registers passed by
// zx_process_start, as well as passing them through to StartLd.  StartLd
// returns this to give it the user entry point address to hand off to.  It
// unwinds the stack to starting conditions and hands off with the same two
// original argument register values, and the third argument register here.
struct StartLdResult {
  uintptr_t entry, third_argument;
};

extern "C" StartLdResult StartLd(zx_handle_t handle, void* vdso) {
  // First thing, bootstrap our own dynamic linking against ourselves and the
  // vDSO.  For this, nothing should go wrong so use a diagnostics object that
  // crashes the process at the first error.  Before linking against the vDSO
  // is completed successfully, there's no way to make a system call to get an
  // error out anyway.
  auto bootstrap_diag = elfldltl::TrapDiagnostics();

  BootstrapModule vdso_module = BootstrapVdsoModule(bootstrap_diag, vdso);
  BootstrapModule self_module = BootstrapSelfModule(bootstrap_diag, vdso_module.module);

  // Only now can we make the system call to discover the page size.
  const size_t page_size = zx_system_get_page_size();
  CompleteBootstrapModule(vdso_module.module, page_size);
  CompleteBootstrapModule(self_module.module, page_size);

  // Read the bootstrap message.
  StartupData startup = ReadBootstrap(zx::unowned_channel{handle});

  // Now that things are bootstrapped, set up the main diagnostics object.
  Diagnostics diag{startup};

  // Start publishing profiling data in an instrumented build.  Before this,
  // the instrumentation is updating counters in the data segment.  After this,
  // it's updating a VMO mapped elsewhere.  That VMO remains mapped after
  // startup just to avoid bothering with code to unmap it since that code and
  // the rest of the return path would have to be uninstrumented.  When the
  // debugdata.vmo_token handle is closed by going out of scope at the end of
  // startup, this will signal the data receiver that the VMO's data is ready.
  // It's still possible for either the last bit of instrumented code in the
  // startup path, or just stray pointer writes in the process after startup
  // will modify it, but will be ignored or will be tolerable noise in the
  // data.  The debugdata.svc_server_end handle is returned to be passed on to
  // the user entry point as its third argument.  Eventually, the user's libc
  // will pass this to ld::Debugdata::Forward.
  ld::Debugdata::Deferred debugdata =
      PublishProfdata(diag, startup.vmar.borrow(), self_module.module.build_id);

  // Set up the allocators.  These objects hold zx::unowned_vmar copies but do
  // not own the VMAR handle.
  auto system_page_allocator = MakeStartupSystemPageAllocator(startup);
  auto scratch = MakeStartupScratchAllocator(system_page_allocator);
  auto initial_exec = MakeStartupInitialExecAllocator(system_page_allocator);

  // TODO(https://fxbug.dev/42084623): We should be making an ldsvc.Config call
  // here to get the correct shared objects.

  // Load the main executable.
  LoadExecutableResult main =
      LoadExecutable(diag, startup, scratch, initial_exec, std::move(startup.executable_vmo));

  auto get_vmo_file = [&diag,
                       &startup](const elfldltl::Soname<>& soname) -> std::optional<VmoFile> {
    if (zx::vmo vmo = startup.GetLibraryVmo(diag, soname.c_str())) {
      return VmoFile{std::move(vmo), diag};
    }
    return {};
  };

  StartupModule::LinkModules(diag, scratch, initial_exec, main.module, get_vmo_file,
                             {vdso_module, self_module}, main.needed_count, startup.vmar);

  // Bail out before relocation if there were any loading errors.
  CheckErrors(diag);

  if constexpr (kProtectData) {
    // Now that startup is completed, protect not only the RELRO, but also all
    // the data and bss.  Then drop that VMAR handle so the protections cannot
    // be changed again.
    ProtectData(diag, page_size, std::move(startup.self_vmar));
  }

  // Bail out before handoff if any errors have been detected.
  CheckErrors(diag);

  return {
      .entry = main.entry,
      // The two arguments to StartLd are the first two arguments to the user
      // entry point, and this is the third.
      .third_argument = debugdata.svc_server_end.release(),
  };
}

}  // namespace ld
