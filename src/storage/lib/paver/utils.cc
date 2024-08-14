// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/utils.h"

#include <dirent.h>
#include <fidl/fuchsia.hardware.block.partition/cpp/wire.h>
#include <fidl/fuchsia.hardware.skipblock/cpp/wire.h>
#include <fidl/fuchsia.sysinfo/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/watcher.h>

#include <string_view>

#include <fbl/algorithm.h>
#include <gpt/gpt.h>

#include "src/lib/uuid/uuid.h"
#include "src/storage/lib/paver/partition-client.h"
#include "src/storage/lib/paver/pave-logging.h"

namespace paver {

namespace {

using uuid::Uuid;

namespace partition = fuchsia_hardware_block_partition;
namespace skipblock = fuchsia_hardware_skipblock;

}  // namespace

// Not static so test can manipulate it.
zx_duration_t g_wipe_timeout = ZX_SEC(3);

std::atomic_uint32_t g_pause_count = 0;

BlockWatcherPauser::~BlockWatcherPauser() {
  if (valid_) {
    const uint32_t old_count = g_pause_count.fetch_sub(1);
    if (old_count == 1) {
      auto result = watcher_->Resume();
      if (result.status() != ZX_OK) {
        ERROR("Failed to unpause the block watcher: %s\n", zx_status_get_string(result.status()));
      } else if (result.value().status != ZX_OK) {
        ERROR("Failed to unpause the block watcher: %s\n",
              zx_status_get_string(result.value().status));
      }
    }
  }
}

zx::result<BlockWatcherPauser> BlockWatcherPauser::Create(
    fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root) {
  auto local = component::ConnectAt<fuchsia_fshost::BlockWatcher>(svc_root);
  if (!local.is_ok()) {
    return local.take_error();
  }

  BlockWatcherPauser pauser(std::move(*local));
  if (auto status = pauser.Pause(); status.is_error()) {
    ERROR("Failed to pause block watcher: %s", status.status_string());
    return status.take_error();
  }

  return zx::ok(std::move(pauser));
}

zx::result<> BlockWatcherPauser::Pause() {
  const uint32_t old_count = g_pause_count.fetch_add(1);
  if (old_count == 0) {
    auto result = watcher_->Pause();
    auto status = zx::make_result(result.ok() ? result.value().status : result.status());

    valid_ = status.is_ok();
    return status;
  }

  valid_ = true;
  return zx::ok();
}

zx::result<std::unique_ptr<VolumeConnector>> OpenBlockPartition(const paver::BlockDevices& devices,
                                                                std::optional<Uuid> unique_guid,
                                                                std::optional<Uuid> type_guid,
                                                                zx_duration_t timeout) {
  ZX_ASSERT(unique_guid || type_guid);

  auto cb = [&](const zx::channel& chan) {
    if (type_guid) {
      auto result = fidl::WireCall(fidl::UnownedClientEnd<partition::Partition>(chan.borrow()))
                        ->GetTypeGuid();
      if (!result.ok()) {
        ERROR("Failed to GetTypeGuid: %s\n", result.status_string());
        return false;
      }
      auto& response = result.value();
      if (response.status != ZX_OK || type_guid != Uuid(response.guid->value.data())) {
        if (response.status != ZX_OK) {
          ERROR("Failed to GetTypeGuid: %s\n", zx_status_get_string(response.status));
        }
        return false;
      }
    }
    if (unique_guid) {
      auto result = fidl::WireCall(fidl::UnownedClientEnd<partition::Partition>(chan.borrow()))
                        ->GetInstanceGuid();
      if (!result.ok()) {
        ERROR("Failed to GetInstanceGuid: %s\n", result.status_string());
        return false;
      }
      const auto& response = result.value();
      if (response.status != ZX_OK || unique_guid != Uuid(response.guid->value.data())) {
        if (response.status != ZX_OK) {
          ERROR("Failed to GetInstanceGuid: %s\n", zx_status_get_string(response.status));
        }
        LOG("Mismatch instance\n");
        return false;
      }
    }
    return true;
  };

  return devices.WaitForPartition(cb, timeout);
}

constexpr char kSkipBlockDevPath[] = "class/skip-block";

zx::result<std::unique_ptr<VolumeConnector>> OpenSkipBlockPartition(
    const paver::BlockDevices& devices, const Uuid& type_guid, zx_duration_t timeout) {
  auto cb = [&](const zx::channel& chan) {
    auto result = fidl::WireCall(fidl::UnownedClientEnd<skipblock::SkipBlock>(chan.borrow()))
                      ->GetPartitionInfo();
    if (!result.ok()) {
      return false;
    }
    const auto& response = result.value();
    return response.status == ZX_OK &&
           type_guid == Uuid(response.partition_info.partition_guid.data());
  };

  return devices.WaitForPartition(cb, timeout, kSkipBlockDevPath);
}

bool HasSkipBlockDevice(const paver::BlockDevices& devices) {
  // Our proxy for detected a skip-block device is by checking for the
  // existence of a device enumerated under the skip-block class.
  return OpenSkipBlockPartition(devices, GUID_ZIRCON_A_VALUE, ZX_SEC(1)).is_ok();
}

// Attempts to open and overwrite the first block of the underlying
// partition. Does not rebind partition drivers.
//
// At most one of |unique_guid| and |type_guid| may be nullptr.
zx::result<> WipeBlockPartition(const paver::BlockDevices& devices, std::optional<Uuid> unique_guid,
                                std::optional<Uuid> type_guid) {
  zx::result partition = OpenBlockPartition(devices, unique_guid, type_guid, g_wipe_timeout);
  if (partition.is_error()) {
    ERROR("Warning: Could not open partition to wipe: %s\n", partition.status_string());
    return partition.take_error();
  }

  // Overwrite the first block to (hackily) ensure the destroyed partition
  // doesn't "reappear" in place.
  zx::result block_partition = BlockPartitionClient::Create(std::move(*partition));
  if (block_partition.is_error()) {
    return block_partition.take_error();
  }
  auto status2 = block_partition->GetBlockSize();
  if (status2.is_error()) {
    ERROR("Warning: Could not get block size of partition: %s\n", status2.status_string());
    return status2.take_error();
  }
  const size_t block_size = status2.value();

  // Rely on vmos being 0 initialized.
  zx::vmo vmo;
  {
    auto status = zx::make_result(
        zx::vmo::create(fbl::round_up(block_size, zx_system_get_page_size()), 0, &vmo));
    if (status.is_error()) {
      ERROR("Warning: Could not create vmo: %s\n", status.status_string());
      return status.take_error();
    }
  }

  if (auto status = block_partition->Write(vmo, block_size); status.is_error()) {
    ERROR("Warning: Could not write to block device: %s\n", status.status_string());
    return status.take_error();
  }

  if (auto status = block_partition->Flush(); status.is_error()) {
    ERROR("Warning: Failed to synchronize block device: %s\n", status.status_string());
    return status.take_error();
  }

  return zx::ok();
}

zx::result<> IsBoard(const fbl::unique_fd& devfs_root, std::string_view board_name) {
  fdio_cpp::UnownedFdioCaller caller(devfs_root.get());
  zx::result status =
      component::ConnectAt<fuchsia_sysinfo::SysInfo>(caller.directory(), "sys/platform");
  if (status.is_error()) {
    return status.take_error();
  }
  fidl::WireResult result = fidl::WireCall(status.value())->GetBoardName();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  fidl::WireResponse response = result.value();
  if (zx_status_t status = response.status; status != ZX_OK) {
    return zx::error(status);
  }
  if (response.name.get() == board_name) {
    return zx::ok();
  }
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> IsBootloader(const fbl::unique_fd& devfs_root, std::string_view vendor) {
  fdio_cpp::UnownedFdioCaller caller(devfs_root.get());
  zx::result status =
      component::ConnectAt<fuchsia_sysinfo::SysInfo>(caller.directory(), "sys/platform");
  if (status.is_error()) {
    return status.take_error();
  }
  fidl::WireResult result = fidl::WireCall(status.value())->GetBootloaderVendor();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  fidl::WireResponse response = result.value();
  if (zx_status_t status = response.status; status != ZX_OK) {
    return zx::error(status);
  }
  if (response.vendor.get() == vendor) {
    return zx::ok();
  }
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}
}  // namespace paver
