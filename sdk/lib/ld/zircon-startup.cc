// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/channel.h>
#include <zircon/assert.h>
#include <zircon/syscalls.h>

#include "bootstrap.h"
#include "diagnostics.h"
#include "zircon.h"

namespace ld {

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

  // TODO(mcgrathr): for now, use the force to conjure a debuglog ex nihilo
  zx::debuglog::create({}, 0, &startup.debuglog);

  // Now that things are bootstrapped, set up the main diagnostics object.
  auto diag = MakeDiagnostics(startup);

  // Bail out before handoff if any errors have been detected.
  CheckErrors(diag);

  diag.report()("Hello world!");

  zx::channel bootstrap{handle};

  char buffer[64];
  uint32_t nbytes, nhandles;
  zx_status_t status = bootstrap.read(0, buffer, nullptr, sizeof(buffer), 0, &nbytes, &nhandles);
  ZX_ASSERT(status == ZX_OK);
  ZX_ASSERT(nhandles == 0);
  return static_cast<int>(nbytes);
}

void ReportError(StartupData& startup, std::string_view str) {
  // If we have a debuglog handle, use that.
  if (startup.debuglog) {
    startup.debuglog.write(0, str.data(), str.size());
  }

  // We might instead (or also?) have a socket, where the messages are easier
  // to capture at the other end.
  if (startup.log_socket) {
    while (!str.empty()) {
      size_t wrote = 0;
      zx_status_t status = startup.log_socket.write(0, str.data(), str.size(), &wrote);
      if (status != ZX_OK) {
        break;
      }
      str.remove_prefix(wrote);
    }
  }
}

}  // namespace ld
