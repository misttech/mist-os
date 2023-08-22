// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/scheduler/role.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/compiler.h>
#include <zircon/process.h>
#include <zircon/processargs.h>

#include "src/storage/minfs/mount.h"

namespace {

int StartComponent() {
  FX_LOGS(INFO) << "starting minfs component";

  // The arguments are either null or don't matter, we collect the real ones later on the startup
  // protocol. What does matter is the DIRECTORY_REQUEST so we can start serving that protocol.
  zx::channel outgoing_server = zx::channel(zx_take_startup_handle(PA_DIRECTORY_REQUEST));
  if (!outgoing_server.is_valid()) {
    FX_LOGS(ERROR) << "PA_DIRECTORY_REQUEST startup handle is required.";
    return EXIT_FAILURE;
  }
  fidl::ServerEnd<fuchsia_io::Directory> outgoing_dir(std::move(outgoing_server));

  zx::channel lifecycle_channel = zx::channel(zx_take_startup_handle(PA_LIFECYCLE));
  if (!lifecycle_channel.is_valid()) {
    FX_LOGS(ERROR) << "PA_LIFECYCLE startup handle is required.";
    return EXIT_FAILURE;
  }
  fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle> lifecycle_request(
      std::move(lifecycle_channel));

  fuchsia_scheduler::SetRoleForThisThread("fuchsia.storage.minfs.main");

  zx::result status = minfs::StartComponent(std::move(outgoing_dir), std::move(lifecycle_request));
  if (status.is_error()) {
    return EXIT_FAILURE;
  }

  return EXIT_SUCCESS;
}

}  // namespace

int main(int argc, char** argv) {
  fuchsia_logging::SetLogSettings({}, {"minfs"});

  if (zx_status_t status = StartComponent(); status != ZX_OK) {
    return EXIT_FAILURE;
  }

  return EXIT_SUCCESS;
}
