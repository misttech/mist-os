// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/testing/ram_disk.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/directory.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/job.h>
#include <lib/zx/time.h>
#include <zircon/syscalls.h>

#include <ramdevice-client/ramdisk.h>

namespace storage {

zx::result<> WaitForRamctl(zx::duration time) {
  if (zx::result channel =
          device_watcher::RecursiveWaitForFile("/dev/sys/platform/ram-disk/ramctl", time);
      channel.is_error()) {
    FX_PLOGS(ERROR, channel.error_value()) << "Failed to wait for for ramctl";
    return channel.take_error();
  }
  return zx::ok();
}

zx::result<RamDisk> RamDisk::Create(int block_size, uint64_t block_count,
                                    const RamDisk::Options& options) {
  ramdisk_client_t* client;
  zx::result<> result;
  if (options.use_v2) {
    ramdisk_options_t ramdisk_options{
        .block_size = static_cast<uint32_t>(block_size),
        .block_count = block_count,
        .type_guid = options.type_guid ? options.type_guid->data() : nullptr,
        .v2 = true,
    };
    result = zx::make_result(ramdisk_create_with_options(&ramdisk_options, &client));
  } else {
    result = WaitForRamctl();
    if (result.is_error()) {
      return result.take_error();
    }
    if (options.type_guid) {
      result = zx::make_result(ramdisk_create_with_guid(
          block_size, block_count, options.type_guid->data(), options.type_guid->size(), &client));
    } else {
      result = zx::make_result(ramdisk_create(block_size, block_count, &client));
    }
  }
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Could not create ramdisk for test: " << result.status_string();
    return result.take_error();
  }
  return zx::ok(RamDisk(client));
}

zx::result<RamDisk> RamDisk::CreateWithVmo(zx::vmo vmo, uint64_t block_size,
                                           const RamDisk::Options& options) {
  ramdisk_client_t* client;
  zx::result<> result;
  if (options.use_v2) {
    ramdisk_options_t ramdisk_options{
        .block_size = static_cast<uint32_t>(block_size),
        .vmo = vmo.release(),
        .v2 = true,
    };
    result = zx::make_result(ramdisk_create_with_options(&ramdisk_options, &client));
  } else {
    auto result = WaitForRamctl();
    if (result.is_error()) {
      return result.take_error();
    }
    result = zx::make_result(ramdisk_create_from_vmo_with_params(vmo.release(), block_size,
                                                                 /*type_guid*/ nullptr,
                                                                 /*guid_len*/ 0, &client));
  }
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Could not create ramdisk for test: " << result.status_string();
    return result.take_error();
  }
  return zx::ok(RamDisk(client));
}

zx::result<fidl::ClientEnd<fuchsia_hardware_block::Block>> RamDisk::channel() const {
  return component::Connect<fuchsia_hardware_block::Block>(path());
}

}  // namespace storage
