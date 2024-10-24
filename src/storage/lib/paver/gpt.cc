// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/gpt.h"

#include <dirent.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <lib/fdio/directory.h>
#include <lib/fit/defer.h>

#include <string_view>

#include <fbl/algorithm.h>
#include <fbl/unique_fd.h>
#include <gpt/c/gpt.h>

#include "src/storage/lib/block_client/cpp/remote_block_device.h"
#include "src/storage/lib/paver/block-devices.h"
#include "src/storage/lib/paver/pave-logging.h"
#include "src/storage/lib/paver/utils.h"

namespace paver {

namespace {

using uuid::Uuid;

namespace block = fuchsia_hardware_block;
namespace device = fuchsia_device;

constexpr size_t ReservedHeaderBlocks(size_t blk_size) {
  constexpr size_t kReservedEntryBlocks{static_cast<size_t>(16) * 1024};
  return (kReservedEntryBlocks + 2 * blk_size) / blk_size;
}

zx::result<> RebindGptDriver(fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
                             block_client::BlockDevice& device) {
  return device.Rebind("gpt.cm");
}

}  // namespace

zx::result<Uuid> GptPartitionType(Partition type, PartitionScheme s) {
  if (s == PartitionScheme::kLegacy) {
    switch (type) {
      case Partition::kBootloaderA:
        return zx::ok(Uuid(GUID_EFI_VALUE));
      case Partition::kZirconA:
        return zx::ok(Uuid(GUID_ZIRCON_A_VALUE));
      case Partition::kZirconB:
        return zx::ok(Uuid(GUID_ZIRCON_B_VALUE));
      case Partition::kZirconR:
        return zx::ok(Uuid(GUID_ZIRCON_R_VALUE));
      case Partition::kVbMetaA:
        return zx::ok(Uuid(GUID_VBMETA_A_VALUE));
      case Partition::kVbMetaB:
        return zx::ok(Uuid(GUID_VBMETA_B_VALUE));
      case Partition::kVbMetaR:
        return zx::ok(Uuid(GUID_VBMETA_R_VALUE));
      case Partition::kAbrMeta:
        return zx::ok(Uuid(GUID_ABR_META_VALUE));
      case Partition::kFuchsiaVolumeManager:
        return zx::ok(Uuid(GUID_FVM_VALUE));
      default:
        ERROR("Partition type is invalid\n");
        return zx::error(ZX_ERR_INVALID_ARGS);
    }
  } else {
    switch (type) {
      case Partition::kBootloaderA:
        return zx::ok(Uuid(GUID_EFI_VALUE));
      case Partition::kZirconA:
      case Partition::kZirconB:
      case Partition::kZirconR:
        return zx::ok(Uuid(GPT_ZIRCON_ABR_TYPE_GUID));
      case Partition::kVbMetaA:
      case Partition::kVbMetaB:
      case Partition::kVbMetaR:
        return zx::ok(Uuid(GPT_VBMETA_ABR_TYPE_GUID));
      case Partition::kAbrMeta:
        return zx::ok(Uuid(GPT_DURABLE_BOOT_TYPE_GUID));
      case Partition::kFuchsiaVolumeManager:
        return zx::ok(Uuid(GPT_FVM_TYPE_GUID));
      default:
        ERROR("Partition type is invalid\n");
        return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }
}

bool FilterByName(const gpt_partition_t& part, std::string_view name) {
  char cstring_name[GPT_NAME_LEN / 2 + 1];
  ::utf16_to_cstring(cstring_name, reinterpret_cast<const uint16_t*>(part.name),
                     sizeof(cstring_name));

  if (name.length() != strnlen(cstring_name, sizeof(cstring_name))) {
    return false;
  }

  // We use a case-insensitive comparison to be compatible with the previous naming scheme.
  // On a ChromeOS device, all of the kernel partitions share a common GUID type, so we
  // distinguish Zircon kernel partitions based on name.
  return strncasecmp(cstring_name, name.data(), name.length()) == 0;
}

bool FilterByTypeAndName(const gpt_partition_t& part, const Uuid& type, std::string_view name) {
  return type == Uuid(part.type) && FilterByName(part, name);
}

zx::result<std::vector<GptDevicePartitioner::GptClients>> GptDevicePartitioner::FindGptDevices(
    const fbl::unique_fd& devfs_root) {
  fbl::unique_fd block_fd;
  if (zx_status_t status =
          fdio_open_fd_at(devfs_root.get(), "class/block", 0, block_fd.reset_and_get_address());
      status != ZX_OK) {
    ERROR("Cannot inspect block devices: %s\n", zx_status_get_string(status));
    return zx::error(status);
  }
  DIR* d = fdopendir(block_fd.duplicate().release());
  if (d == nullptr) {
    ERROR("Cannot inspect block devices: %s\n", strerror(errno));
    return zx::error(ZX_ERR_INTERNAL);
  }
  const auto closer = fit::defer([d]() { closedir(d); });
  fdio_cpp::FdioCaller block_caller(std::move(block_fd));

  struct dirent* de;
  std::vector<GptClients> found_devices;
  while ((de = readdir(d)) != nullptr) {
    if (std::string_view{de->d_name} == ".") {
      continue;
    }
    zx::result block_endpoints = fidl::CreateEndpoints<fuchsia_hardware_block::Block>();
    if (block_endpoints.is_error()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    if (zx_status_t status =
            fdio_service_connect_at(block_caller.borrow_channel(), de->d_name,
                                    block_endpoints->server.TakeChannel().release());
        status != ZX_OK) {
      ERROR("Cannot connect %s: %s\n", de->d_name, zx_status_get_string(status));
      continue;
    }
    {
      const fidl::WireResult result = fidl::WireCall(block_endpoints->client)->GetInfo();
      if (!result.ok()) {
        ERROR("Cannot get block info from %s: %s\n", de->d_name,
              result.FormatDescription().c_str());
        continue;
      }
      const fit::result response = result.value();
      if (response.is_error()) {
        ERROR("Cannot get block info from %s: %s\n", de->d_name,
              zx_status_get_string(response.error_value()));
        continue;
      }
      if (response.value()->info.flags & fuchsia_hardware_block::wire::Flag::kRemovable) {
        continue;
      }
    }

    zx::result controller_endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
    if (controller_endpoints.is_error()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    std::string controller_path = std::string(de->d_name) + "/device_controller";
    if (zx_status_t status =
            fdio_service_connect_at(block_caller.borrow_channel(), controller_path.c_str(),
                                    controller_endpoints->server.TakeChannel().release());
        status != ZX_OK) {
      ERROR("Cannot connect %s: %s\n", de->d_name, zx_status_get_string(status));
      continue;
    }
    const fidl::WireResult result =
        fidl::WireCall(controller_endpoints->client)->GetTopologicalPath();
    if (!result.ok()) {
      ERROR("Cannot get topological path from %s: %s\n", de->d_name,
            result.FormatDescription().c_str());
      continue;
    }
    const fit::result response = result.value();
    if (response.is_error()) {
      ERROR("Cannot get topological path from %s: %s\n", de->d_name,
            zx_status_get_string(response.error_value()));
      continue;
    }

    std::string path_str(response.value()->path.get());

    // The GPT which will be a non-removable block device that isn't a partition or fvm created
    // partition itself.
    if (path_str.find("part-") == std::string::npos &&
        path_str.find("/fvm/") == std::string::npos) {
      found_devices.emplace_back(GptClients{
          .topological_path = path_str,
          .block = std::move(block_endpoints->client),
          .controller = std::move(controller_endpoints->client),
      });
    }
  }

  if (found_devices.empty()) {
    ERROR("No candidate GPT found\n");
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  return zx::ok(std::move(found_devices));
}

zx::result<std::unique_ptr<GptDevicePartitioner>> GptDevicePartitioner::InitializeProvidedGptDevice(
    const paver::BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
    fidl::UnownedClientEnd<fuchsia_device::Controller> gpt_device) {
  // Connect to the volume protocol.
  zx::result volume_endpoints = fidl::CreateEndpoints<fuchsia_hardware_block_volume::Volume>();
  if (volume_endpoints.is_error()) {
    ERROR("Warning: failed to create block endpoints: %s\n", volume_endpoints.status_string())
    return volume_endpoints.take_error();
  }
  auto& [device, volume_server] = volume_endpoints.value();
  if (fidl::OneWayError response =
          fidl::WireCall(gpt_device)->ConnectToDeviceFidl(volume_server.TakeChannel());
      !response.ok()) {
    ERROR("Warning: failed to connect to GPT block protocol: %s\n",
          response.FormatDescription().c_str());
    return zx::error(response.status());
  }

  // Connect to the controller protocol.
  zx::result controller_endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
  if (controller_endpoints.is_error()) {
    ERROR("Warning: failed to create controller endpoints: %s\n",
          controller_endpoints.status_string())
    return controller_endpoints.take_error();
  }
  auto& [controller, controller_server] = controller_endpoints.value();
  if (fidl::OneWayError response =
          fidl::WireCall(gpt_device)->ConnectToController(std::move(controller_server));
      !response.ok()) {
    ERROR("Warning: failed to connect to GPT controller protocol: %s\n",
          response.FormatDescription().c_str());
    return zx::error(response.status());
  }

  const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
  if (!result.ok()) {
    ERROR("Warning: Could not acquire GPT block info: %s\n", result.FormatDescription().c_str());
    return zx::error(result.status());
  }
  fit::result response = result.value();
  if (response.is_error()) {
    ERROR("Warning: Could not acquire GPT block info: %s\n",
          zx_status_get_string(response.error_value()));
    return response.take_error();
  }
  const fuchsia_hardware_block::wire::BlockInfo& info = response.value()->info;

  zx::result remote_device =
      block_client::RemoteBlockDevice::Create(std::move(device), std::move(controller));
  if (!remote_device.is_ok()) {
    return remote_device.take_error();
  }
  zx::result gpt_result =
      GptDevice::Create(std::move(remote_device.value()), info.block_size, info.block_count);
  if (gpt_result.is_error()) {
    ERROR("Failed to get GPT info: %s\n", gpt_result.status_string());
    return zx::error(ZX_ERR_BAD_STATE);
  }
  std::unique_ptr<GptDevice>& gpt = gpt_result.value();

  if (!gpt->Valid()) {
    ERROR("Located GPT is invalid; Attempting to initialize\n");
    if (gpt->RemoveAllPartitions() != ZX_OK) {
      ERROR("Failed to create empty GPT\n");
      return zx::error(ZX_ERR_BAD_STATE);
    }
    if (gpt->Sync() != ZX_OK) {
      ERROR("Failed to sync empty GPT\n");
      return zx::error(ZX_ERR_BAD_STATE);
    }
    if (zx::result status = RebindGptDriver(svc_root, gpt->device()); status.is_error()) {
      ERROR("Failed to rebind GPT\n");
      return status.take_error();
    }
    LOG("Rebound GPT driver successfully\n");
  }

  return zx::ok(new GptDevicePartitioner(devices.Duplicate(), svc_root, std::move(gpt), info));
}

zx::result<GptDevicePartitioner::InitializeGptResult> GptDevicePartitioner::InitializeGpt(
    const paver::BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
    fidl::ClientEnd<fuchsia_device::Controller> block_controller) {
  if (block_controller) {
    zx::result status = InitializeProvidedGptDevice(devices, svc_root, block_controller);
    if (status.is_error()) {
      return status.take_error();
    }
    return zx::ok(InitializeGptResult{std::move(status.value()), false});
  }

  zx::result gpt_devices = FindGptDevices(devices.devfs_root());
  if (gpt_devices.is_error()) {
    ERROR("Failed to find GPT: %s\n", gpt_devices.status_string());
    return gpt_devices.take_error();
  }

  std::vector<fidl::ClientEnd<fuchsia_device::Controller>> non_removable_gpt_devices;

  std::unique_ptr<GptDevicePartitioner> gpt_partitioner;
  for (auto& gpt_device : gpt_devices.value()) {
    const fidl::WireResult result = fidl::WireCall(gpt_device.block)->GetInfo();
    if (!result.ok()) {
      ERROR("Warning: Could not acquire GPT block info: %s\n", result.FormatDescription().c_str());
      return zx::error(result.status());
    }
    fit::result response = result.value();
    if (response.is_error()) {
      ERROR("Warning: Could not acquire GPT block info: %s\n",
            zx_status_get_string(response.error_value()));
      return response.take_error();
    }

    const fuchsia_hardware_block::wire::BlockInfo& info = response.value()->info;
    if (info.flags & block::wire::Flag::kRemovable) {
      continue;
    }

    auto [controller, controller_server] = fidl::Endpoints<fuchsia_device::Controller>::Create();
    if (fidl::OneWayStatus status = fidl::WireCall(gpt_device.controller)
                                        ->ConnectToController(std::move(controller_server));
        !status.ok()) {
      ERROR("Failed to connect to new controller %s\n", status.FormatDescription().c_str());
      continue;
    }

    zx::result remote_device = block_client::RemoteBlockDevice::Create(
        fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>(gpt_device.block.TakeChannel()),
        std::move(gpt_device.controller));
    if (!remote_device.is_ok()) {
      return remote_device.take_error();
    }
    zx::result gpt_result =
        GptDevice::Create(std::move(remote_device.value()), info.block_size, info.block_count);
    if (gpt_result.is_error()) {
      ERROR("Failed to get GPT info: %s\n", gpt_result.status_string());
      return zx::error(ZX_ERR_BAD_STATE);
    }
    std::unique_ptr<GptDevice>& gpt = gpt_result.value();

    if (!gpt->Valid()) {
      continue;
    }

    non_removable_gpt_devices.push_back(std::move(controller));

    auto partitioner =
        WrapUnique(new GptDevicePartitioner(devices.Duplicate(), svc_root, std::move(gpt), info));

    if (partitioner->FindPartition(IsFvmOrAndroidPartition).is_error()) {
      continue;
    }

    if (gpt_partitioner) {
      ERROR("Found multiple block devices with valid GPTs. Unsuppported.\n");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
    gpt_partitioner = std::move(partitioner);
  }

  if (gpt_partitioner) {
    return zx::ok(InitializeGptResult{std::move(gpt_partitioner), false});
  }

  if (non_removable_gpt_devices.size() == 1) {
    // If we only find a single non-removable gpt device, we initialize it's partition table.
    auto status = InitializeProvidedGptDevice(devices, svc_root, non_removable_gpt_devices[0]);
    if (status.is_error()) {
      return status.take_error();
    }
    return zx::ok(InitializeGptResult{std::move(status.value()), true});
  }

  ERROR(
      "Unable to find a valid GPT on this device with the expected partitions. "
      "Please run *one* of the following command(s):\n");

  for (const auto& device : gpt_devices.value()) {
    ERROR("fx init-partition-tables %s\n", device.topological_path.c_str());
  }

  return zx::error(ZX_ERR_NOT_FOUND);
}

struct PartitionPosition {
  size_t start;   // Block, inclusive
  size_t length;  // In Blocks
};

zx::result<GptDevicePartitioner::FindFirstFitResult> GptDevicePartitioner::FindFirstFit(
    size_t bytes_requested) const {
  LOG("Looking for space\n");
  // Gather GPT-related information.
  size_t blocks_requested = (bytes_requested + block_info_.block_size - 1) / block_info_.block_size;

  // Sort all partitions by starting block.
  // For simplicity, include the 'start' and 'end' reserved spots as
  // partitions.
  size_t partition_count = 0;
  PartitionPosition partitions[gpt::kPartitionCount + 2];
  const size_t reserved_blocks = ReservedHeaderBlocks(block_info_.block_size);
  partitions[partition_count].start = 0;
  partitions[partition_count++].length = reserved_blocks;
  partitions[partition_count].start = block_info_.block_count - reserved_blocks;
  partitions[partition_count++].length = reserved_blocks;

  for (uint32_t i = 0; i < gpt::kPartitionCount; i++) {
    zx::result<const gpt_partition_t*> p = gpt_->GetPartition(i);
    if (p.is_error()) {
      continue;
    }
    partitions[partition_count].start = (*p)->first;
    partitions[partition_count].length = (*p)->last - (*p)->first + 1;
    LOG("Partition seen with start %zu, end %zu (length %zu)\n", (*p)->first, (*p)->last,
        partitions[partition_count].length);
    partition_count++;
  }
  LOG("Sorting\n");
  qsort(partitions, partition_count, sizeof(PartitionPosition), [](const void* p1, const void* p2) {
    ssize_t s1 = static_cast<ssize_t>(static_cast<const PartitionPosition*>(p1)->start);
    ssize_t s2 = static_cast<ssize_t>(static_cast<const PartitionPosition*>(p2)->start);
    return s1 == s2 ? 0 : (s1 > s2 ? +1 : -1);
  });

  // Look for space between the partitions. Since the reserved spots of the
  // GPT were included in |partitions|, all available space will be located
  // "between" partitions.
  for (size_t i = 0; i < partition_count - 1; i++) {
    const size_t next = partitions[i].start + partitions[i].length;
    LOG("Partition[%zu] From Block [%zu, %zu) ... (next partition starts at block %zu)\n", i,
        partitions[i].start, next, partitions[i + 1].start);

    if (next > partitions[i + 1].start) {
      ERROR("Corrupted GPT\n");
      return zx::error(ZX_ERR_IO);
    }
    const size_t free_blocks = partitions[i + 1].start - next;
    LOG("    There are %zu free blocks (%zu requested)\n", free_blocks, blocks_requested);
    if (free_blocks >= blocks_requested) {
      return zx::ok(FindFirstFitResult{next, free_blocks});
    }
  }
  ERROR("No GPT space found\n");
  return zx::error(ZX_ERR_NO_RESOURCES);
}

zx::result<Uuid> GptDevicePartitioner::CreateGptPartition(const char* name, const Uuid& type,
                                                          uint64_t offset, uint64_t blocks) const {
  Uuid guid = Uuid::Generate();

  if (zx_status_t status =
          gpt_->AddPartition(name, type.bytes(), guid.bytes(), offset, blocks, 0).status_value();
      status != ZX_OK) {
    ERROR("Failed to add partition: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  if (zx_status_t status = gpt_->Sync(); status != ZX_OK) {
    ERROR("Failed to sync GPT: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  if (auto status = zx::make_result(gpt_->ClearPartition(offset, 1)); status.is_error()) {
    ERROR("Failed to clear first block of new partition: %s", status.status_string());
    return status.take_error();
  }
  if (zx::result status = RebindGptDriver(svc_root_, gpt_->device()); status.is_error()) {
    ERROR("Failed to rebind GPT: %s", status.status_string());
    return zx::error(ZX_ERR_BAD_STATE);
  }

  return zx::ok(guid);
}

zx::result<std::unique_ptr<PartitionClient>> GptDevicePartitioner::AddPartition(
    const char* name, const Uuid& type, size_t minimum_size_bytes,
    size_t optional_reserve_bytes) const {
  auto status = FindFirstFit(minimum_size_bytes);
  if (status.is_error()) {
    ERROR("Couldn't find fit\n");
    return status.take_error();
  }
  const size_t start = status->start;
  size_t length = status->length;
  LOG("Found space in GPT - OK %zu @ %zu\n", length, start);

  if (optional_reserve_bytes) {
    // If we can fulfill the requested size, and we still have space for the
    // optional reserve section, then we should shorten the amount of blocks
    // we're asking for.
    //
    // This isn't necessary, but it allows growing the GPT later, if necessary.
    const size_t optional_reserve_blocks = optional_reserve_bytes / block_info_.block_size;
    if (length - optional_reserve_bytes > (minimum_size_bytes / block_info_.block_size)) {
      LOG("Space for reserve - OK\n");
      length -= optional_reserve_blocks;
    }
  } else {
    length = fbl::round_up(minimum_size_bytes, block_info_.block_size) / block_info_.block_size;
  }
  LOG("Final space in GPT - OK %zu @ %zu\n", length, start);

  auto status_or_guid = CreateGptPartition(name, type, start, length);
  if (status_or_guid.is_error()) {
    return status_or_guid.take_error();
  }
  LOG("Added partition, waiting for bind\n");

  auto status_or_part = OpenBlockPartition(devices_, status_or_guid.value(), type, ZX_SEC(15));
  if (status_or_part.is_error()) {
    ERROR("Added partition, waiting for bind - NOT FOUND\n");
    return status_or_part.take_error();
  }

  LOG("Added partition, waiting for bind - OK\n");
  return BlockPartitionClient::Create(std::move(status_or_part.value()));
}

zx::result<GptDevicePartitioner::FindPartitionResult> GptDevicePartitioner::FindPartition(
    FilterCallback filter) const {
  for (uint32_t i = 0; i < gpt::kPartitionCount; i++) {
    zx::result<gpt_partition_t*> p = gpt_->GetPartition(i);
    if (p.is_error()) {
      continue;
    }
    if (filter(**p)) {
      LOG("Found partition in GPT, partition %u\n", i);
      auto status = OpenBlockPartition(devices_, Uuid((*p)->guid), Uuid((*p)->type), ZX_SEC(5));
      if (status.is_error()) {
        ERROR("Couldn't open partition: %s\n", status.status_string());
        return status.take_error();
      }
      zx::result part = BlockPartitionClient::Create(std::move(status.value()));
      if (part.is_error()) {
        return part.take_error();
      }
      return zx::ok(FindPartitionResult{std::move(*part), *p});
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<> GptDevicePartitioner::WipePartitions(FilterCallback filter) const {
  bool modify = false;
  for (uint32_t i = 0; i < gpt::kPartitionCount; i++) {
    zx::result<const gpt_partition_t*> p = gpt_->GetPartition(i);
    if (p.is_error() || !filter(**p)) {
      continue;
    }

    modify = true;

    // Ignore the return status; wiping is a best-effort approach anyway.
    static_cast<void>(WipeBlockPartition(devices_, Uuid((*p)->guid), Uuid((*p)->type)));

    if (gpt_->RemovePartition((*p)->guid) != ZX_OK) {
      ERROR("Warning: Could not remove partition\n");
    } else {
      // If we successfully clear the partition, then all subsequent
      // partitions get shifted down. If we just deleted partition 'i',
      // we now need to look at partition 'i' again, since it's now
      // occupied by what was in 'i+1'.
      i--;
    }
  }
  if (modify) {
    gpt_->Sync();
    LOG("Immediate reboot strongly recommended\n");
  }
  static_cast<void>(RebindGptDriver(svc_root_, gpt_->device()));
  return zx::ok();
}

zx::result<> GptDevicePartitioner::WipeFvm() const {
  return WipeBlockPartition(devices_, std::nullopt, Uuid(GUID_FVM_VALUE));
}

zx::result<> GptDevicePartitioner::WipePartitionTables() const {
  return WipePartitions([](const gpt_partition_t&) { return true; });
}

}  // namespace paver
