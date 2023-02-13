// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/fshost/nand-device.h"

#include <fidl/fuchsia.device/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/syslog/cpp/macros.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#include <memory>

#include "src/storage/fshost/block-device.h"

namespace fshost {

zx::result<std::unique_ptr<BlockDeviceInterface>> NandDevice::OpenBlockDevice(
    const char* topological_path) const {
  zx::result device = component::Connect<fuchsia_hardware_block::Block>(topological_path);
  if (device.is_error()) {
    FX_PLOGS(WARNING, device.error_value()) << "Failed to open block device " << topological_path;
    return device.take_error();
  }
  return zx::ok(std::make_unique<NandDevice>(mounter_, std::move(device.value()), device_config_));
}

}  // namespace fshost
