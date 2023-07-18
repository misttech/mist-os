// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/note.h>
#include <lib/elfldltl/phdr.h>
#include <lib/elfldltl/symbol.h>
#include <lib/ld/module.h>
#include <lib/trivial-allocator/new.h>
#include <lib/trivial-allocator/posix.h>
#include <sys/uio.h>
#include <unistd.h>

#include <array>
#include <cassert>
#include <cstring>

#include "allocator.h"
#include "bootstrap.h"
#include "diagnostics.h"
#include "posix.h"

namespace ld {

using Phdr = abi::Abi<>::Phdr;

// This gets called from the entry-point written in assembly, which just
// passes the incoming SP value as the first argument.
extern "C" uintptr_t StartLd(StartupStack& stack) {
  StartupData startup = {
      .argc = stack.argc,
      .argv = stack.argv,
      .envp = stack.envp(),
  };

  // First scan the auxv for important pointers and values.
  uintptr_t phdr = 0, phnum = 0, entry = 0;
  const void* vdso = nullptr;
  for (const auto* av = stack.GetAuxv(); av->front() != 0; ++av) {
    const auto& [tag, value] = *av;
    switch (static_cast<AuxvTag>(tag)) {
      case AuxvTag::kPagesz:
        startup.page_size = value;
        break;
      case AuxvTag::kEntry:
        entry = value;
        break;
      case AuxvTag::kSysinfoEhdr:
        vdso = reinterpret_cast<const void*>(value);
        break;
      case AuxvTag::kPhdr:
        phdr = value;
        break;
      case AuxvTag::kPhnum:
        phnum = value;
        break;
      case AuxvTag::kPhent:
        assert(value == sizeof(Phdr));
        break;
      default:
        break;
    }
  }
  cpp20::span phdrs{reinterpret_cast<const Phdr*>(phdr), phnum};

  // TODO(mcgrathr): used later for real PT_INTERP behavior
  std::ignore = entry;
  std::ignore = phdr;
  std::ignore = phnum;

  // First thing, bootstrap our own dynamic linking against ourselves and the
  // vDSO.  For this, nothing should go wrong so use a diagnostics object that
  // crashes the process at the first error.  But we can still use direct
  // system calls to write error messages.
  auto bootstrap_diag = elfldltl::Diagnostics{
      MakeDiagnosticsReport(startup),
      elfldltl::DiagnosticsPanicFlags{},
  };

  abi::Abi<>::Module vdso_module;
  if (vdso) {
    // If there is no vDSO, then there will just be empty symbols to link
    // against and no references can resolve to any vDSO-defined symbols.
    vdso_module = BootstrapVdsoModule(bootstrap_diag, vdso, startup.page_size);
  }
  auto self_module = BootstrapSelfModule(bootstrap_diag, vdso_module);
  CompleteBootstrapModule(self_module, startup.page_size);

  // Now that things are bootstrapped, set up the main diagnostics object.
  auto diag = MakeDiagnostics(startup);

  // Set up the allocators.
  trivial_allocator::PosixMmap system_page_allocator{startup.page_size};
  auto scratch = MakeScratchAllocator(system_page_allocator);
  auto initial_exec = MakeInitialExecAllocator(system_page_allocator);

  // Bail out before handoff if any errors have been detected.
  CheckErrors(diag);

  int result = 0;
  for (uintptr_t i = 0; i < startup.argc; ++i) {
    diag.report()(startup.argv[i]);
    result += static_cast<int>(strlen(startup.argv[i]));
  }

  return result;
}

void ReportError(StartupData& startup, std::string_view str) {
  const std::array iov = {
      iovec{const_cast<char*>(str.data()), str.size()},
      iovec{const_cast<char*>("\n"), 1},
  };
  writev(STDERR_FILENO, iov.data(), iov.size());
}

}  // namespace ld
