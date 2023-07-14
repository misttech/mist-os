// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>
#include <lib/zx/resource.h>
#include <lib/zx/result.h>
#include <zircon/process.h>
#include <zircon/processargs.h>

#include "src/storage/bin/blobfs/blobfs_component_config.h"
#include "src/storage/blobfs/mount.h"

namespace {

zx::resource AttemptToGetVmexResource() {
  auto client_end_or = component::Connect<fuchsia_kernel::VmexResource>();
  if (client_end_or.is_error()) {
    FX_LOGS(WARNING) << "Failed to connect to fuchsia.kernel.VmexResource: "
                     << client_end_or.status_string();
    return zx::resource();
  }

  auto result = fidl::WireCall(*client_end_or)->Get();
  if (!result.ok()) {
    FX_LOGS(WARNING) << "fuchsia.kernel.VmexResource.Get() failed: " << result.error();
    return zx::resource();
  }

  return std::move(result.value().resource);
}

zx_status_t StartComponent() {
  FX_LOGS(INFO) << "starting blobfs component";

  zx::channel outgoing_server = zx::channel(zx_take_startup_handle(PA_DIRECTORY_REQUEST));
  if (!outgoing_server.is_valid()) {
    FX_LOGS(ERROR) << "PA_DIRECTORY_REQUEST startup handle is required.";
    return ZX_ERR_INTERNAL;
  }
  fidl::ServerEnd<fuchsia_io::Directory> outgoing_dir(std::move(outgoing_server));

  zx::channel lifecycle_channel = zx::channel(zx_take_startup_handle(PA_LIFECYCLE));
  if (!lifecycle_channel.is_valid()) {
    FX_LOGS(ERROR) << "PA_LIFECYCLE startup handle is required.";
    return ZX_ERR_INTERNAL;
  }
  fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle> lifecycle_request(
      std::move(lifecycle_channel));

  zx::resource vmex = AttemptToGetVmexResource();
  if (!vmex.is_valid()) {
    FX_LOGS(WARNING) << "VMEX resource unavailable, executable blobs are unsupported";
  }

  auto config = blobfs_component_config::Config::TakeFromStartupHandle();
  const blobfs::ComponentOptions options{
      .pager_threads = config.pager_threads(),
  };
  // blocks until blobfs exits
  zx::result status = blobfs::StartComponent(options, std::move(outgoing_dir),
                                             std::move(lifecycle_request), std::move(vmex));
  if (status.is_error()) {
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

}  // namespace

int main(int argc, char** argv) {
  fuchsia_logging::SetLogSettings({}, {"blobfs"});

  if (zx_status_t status = StartComponent(); status != ZX_OK) {
    return EXIT_FAILURE;
  }

  return EXIT_SUCCESS;
}
