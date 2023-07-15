// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/diagnostics.h>

#include <cstring>

#include "bootstrap.h"

namespace ld {

extern "C" int _start(const char* str) {
  constexpr size_t page_size = 4096;  // TODO(mcgrathr): get from auxv

  // First thing, bootstrap our own dynamic linking against ourselves and
  // the vDSO.  For this, nothing should go wrong so use a diagnostics
  // object that crashes the process at the first error.
  //
  // TODO(mcgrathr): for now, just trap silently. when we wire up a report
  // function using syscalls, this can just use that with panic flags so it
  // writes messages.
  auto bootstrap_diag = elfldltl::TrapDiagnostics();

  abi::Abi<>::Module vdso_module;
  // TODO(mcgrathr): get the vdso from auxv
  // vdso_module = BootstrapVdsoModule(bootstrap_diag, vdso, page_size);

  auto self_module = BootstrapSelfModule(bootstrap_diag, vdso_module);
  CompleteBootstrapModule(self_module, page_size);

  return static_cast<int>(strlen(str));
}

}  // namespace ld
