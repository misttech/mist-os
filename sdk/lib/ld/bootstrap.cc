// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bootstrap.h"

#include <lib/elfldltl/diagnostics.h>
#include <zircon/syscalls.h>

namespace ld {

// TODO(fxbug.dev/130483): _start should normally be `[[noreturn]] void` though for the timebeing
// it returns an int for easy testing.
extern "C" int _start(zx_handle_t handle, void* vdso) {
  auto diag = elfldltl::TrapDiagnostics();
  auto vdso_module = BootstrapVdsoModule(diag, vdso);
  auto self_module = BootstrapSelfModule(diag, vdso_module);
  // Only now can we make the system call to discover the page size.
  const size_t page_size = zx_system_get_page_size();
  CompleteBootstrapModule(vdso_module, page_size);
  CompleteBootstrapModule(self_module, page_size);

  int array[2];
  zx_channel_read(handle, 0, &array, nullptr, sizeof(array), 0, nullptr, nullptr);
  return array[0] + array[1];
}

}  // namespace ld
