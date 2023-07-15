// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/diagnostics.h>
#include <lib/zx/channel.h>
#include <zircon/assert.h>
#include <zircon/syscalls.h>

#include "bootstrap.h"

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

  zx::channel bootstrap{handle};

  char buffer[64];
  uint32_t nbytes, nhandles;
  zx_status_t status = bootstrap.read(0, buffer, nullptr, sizeof(buffer), 0, &nbytes, &nhandles);
  ZX_ASSERT(status == ZX_OK);
  ZX_ASSERT(nhandles == 0);
  return static_cast<int>(nbytes);
}

}  // namespace ld
