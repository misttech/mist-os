// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.nand/cpp/wire.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/unsafe.h>
#include <lib/fdio/watcher.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <zircon/types.h>

#include <utility>

#include <fbl/string_buffer.h>
#include <fbl/unique_fd.h>
#include <ramdevice-client-test/ramnandctl.h>

namespace ramdevice_client_test {

__EXPORT
zx_status_t RamNandCtl::Create(fbl::unique_fd devfs_root, std::unique_ptr<RamNandCtl>* out) {
  zx::result channel =
      device_watcher::RecursiveWaitForFile(devfs_root.get(), "sys/platform/00:00:2e/nand-ctl");
  if (channel.is_error()) {
    fprintf(stderr, "ram_nand_ctl device failed enumerated: %s\n", channel.status_string());
    return channel.status_value();
  }
  fidl::ClientEnd<fuchsia_hardware_nand::RamNandCtl> client_end(std::move(channel.value()));

  *out = std::unique_ptr<RamNandCtl>(new RamNandCtl(std::move(devfs_root), std::move(client_end)));
  return ZX_OK;
}

__EXPORT
zx_status_t RamNandCtl::CreateRamNand(fuchsia_hardware_nand::wire::RamNandInfo config,
                                      std::optional<ramdevice_client::RamNand>* out) const {
  const fidl::WireResult result = fidl::WireCall(ctl())->CreateDevice(std::move(config));
  if (!result.ok()) {
    fprintf(stderr, "Could not create ram_nand device: %s\n", result.status_string());
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  if (zx_status_t status = response.status; status != ZX_OK) {
    fprintf(stderr, "Could not create ram_nand device: %s\n", zx_status_get_string(status));
    return status;
  }

  fbl::String path = fbl::String::Concat({
      "sys/platform/00:00:2e/nand-ctl/",
      response.name.get(),
  });
  fprintf(stderr, "Trying to open (%s)\n", path.c_str());

  std::string controller_path = std::string(path.c_str()) + "/device_controller";
  zx::result channel =
      device_watcher::RecursiveWaitForFile(devfs_root_.get(), controller_path.c_str());
  if (channel.is_error()) {
    fprintf(stderr, "Could not open ram_nand device (%s): %s\n", path.c_str(),
            channel.status_string());
    return channel.status_value();
  }
  fidl::ClientEnd<fuchsia_device::Controller> client_end(std::move(channel.value()));

  *out = ramdevice_client::RamNand(std::move(client_end));
  return ZX_OK;
}

}  // namespace ramdevice_client_test
