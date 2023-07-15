// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/diagnostics.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/note.h>
#include <lib/elfldltl/phdr.h>
#include <lib/elfldltl/symbol.h>
#include <lib/ld/module.h>

#include <cassert>
#include <cstring>

#include "bootstrap.h"
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

  // First thing, bootstrap our own dynamic linking against ourselves and
  // the vDSO.  For this, nothing should go wrong so use a diagnostics
  // object that crashes the process at the first error.
  //
  // TODO(mcgrathr): for now, just trap silently. when we wire up a report
  // function using syscalls, this can just use that with panic flags so it
  // writes messages.
  auto bootstrap_diag = elfldltl::TrapDiagnostics();

  abi::Abi<>::Module vdso_module;
  if (vdso) {
    // If there is no vDSO, then there will just be empty symbols to link
    // against and no references can resolve to any vDSO-defined symbols.
    vdso_module = BootstrapVdsoModule(bootstrap_diag, vdso, startup.page_size);
  }
  auto self_module = BootstrapSelfModule(bootstrap_diag, vdso_module);
  CompleteBootstrapModule(self_module, startup.page_size);

  return static_cast<int>(strlen(startup.argv[0]));
}

}  // namespace ld
