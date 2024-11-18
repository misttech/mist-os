// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/gpt.h"

#include <dirent.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.fshost/cpp/wire.h>
#include <fidl/fuchsia.storagehost/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/status.h>
#include <lib/fit/defer.h>
#include <lib/zx/result.h>

#include <algorithm>
#include <string_view>

#include <fbl/algorithm.h>
#include <fbl/unique_fd.h>
#include <gpt/c/gpt.h>

#include "src/storage/lib/block_client/cpp/remote_block_device.h"
#include "src/storage/lib/paver/block-devices.h"
#include "src/storage/lib/paver/pave-logging.h"
#include "src/storage/lib/paver/utils.h"
#include "zircon/status.h"

namespace paver {

namespace {

using uuid::Uuid;

constexpr size_t ReservedHeaderBlocks(size_t blk_size) {
  constexpr size_t kReservedEntryBlocks{static_cast<size_t>(16) * 1024};
  return (kReservedEntryBlocks + 2 * blk_size) / blk_size;
}

zx::result<> RebindGptDriver(fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
                             block_client::BlockDevice& device) {
  return device.Rebind("gpt.cm");
}

zx::result<GptPartitionMetadata> QueryGptPartitionMetadata(
    fidl::UnownedClientEnd<fuchsia_hardware_block_partition::Partition> volume) {
  using fuchsia_hardware_block_partition::Partition;
  GptPartitionMetadata metadata;

  fidl::WireResult result = fidl::WireCall<Partition>(volume)->GetMetadata();
  if (!result.ok() || result->is_error()) {
    return zx::error(result.status());
  }
  if (!result.value()->has_name() || !result.value()->has_type_guid() ||
      !result.value()->has_instance_guid()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  return zx::ok(GptPartitionMetadata{
      .name = std::string(result.value()->name().cbegin(), result.value()->name().cend()),
      .type_guid = Uuid(result.value()->type_guid().value.data()),
      .instance_guid = Uuid(result.value()->instance_guid().value.data()),
  });
}

zx::result<std::unique_ptr<GptDevice>> ConnectToGpt(
    fidl::ClientEnd<fuchsia_device::Controller> controller) {
  // Connect to the volume protocol.
  zx::result volume_endpoints = fidl::CreateEndpoints<fuchsia_hardware_block_volume::Volume>();
  if (volume_endpoints.is_error()) {
    ERROR("Warning: failed to create block endpoints: %s\n", volume_endpoints.status_string())
    return volume_endpoints.take_error();
  }
  auto& [device, volume_server] = volume_endpoints.value();
  if (fidl::OneWayError response =
          fidl::WireCall(controller)->ConnectToDeviceFidl(volume_server.TakeChannel());
      !response.ok()) {
    ERROR("Warning: failed to connect to GPT block protocol: %s\n",
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
  return GptDevice::Create(std::move(remote_device.value()), info.block_size, info.block_count);
}

}  // namespace

using PartitionInitSpec = GptDevicePartitioner::PartitionInitSpec;

PartitionInitSpec PartitionInitSpec::ForKnownPartition(Partition partition, PartitionScheme scheme,
                                                       size_t size_bytes) {
  const char* name = PartitionName(partition, scheme);
  std::optional<Uuid> type = PartitionTypeGuid(partition, scheme);
  ZX_ASSERT(name && type);
  return PartitionInitSpec{
      .name = name,
      .type = *type,
      .start_block = 0,
      .size_bytes = size_bytes,
  };
}

bool FilterByName(const GptPartitionMetadata& part, std::string_view name) {
  if (name.length() != part.name.length()) {
    return false;
  }
  // We use a case-insensitive comparison to be compatible with the previous naming scheme.
  // On a ChromeOS device, all of the kernel partitions share a common GUID type, so we
  // distinguish Zircon kernel partitions based on name.
  return strncasecmp(part.name.data(), name.data(), name.length()) == 0;
}

bool FilterByTypeAndName(const GptPartitionMetadata& part, const Uuid& type,
                         std::string_view name) {
  return type == part.type_guid && FilterByName(part, name);
}

zx::result<std::vector<GptDevicePartitioner::GptClients>> GptDevicePartitioner::FindGptDevices(
    const fbl::unique_fd& devfs_root) {
  fbl::unique_fd block_fd;
  if (zx_status_t status =
          fdio_open3_fd_at(devfs_root.get(), "class/block", 0, block_fd.reset_and_get_address());
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

  zx::result gpt_result = ConnectToGpt(std::move(controller));
  if (gpt_result.is_error()) {
    ERROR("Failed to connect to GPT: %s\n", gpt_result.status_string());
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

  return zx::ok(new GptDevicePartitioner(devices.Duplicate(), svc_root, gpt->TotalBlockCount(),
                                         static_cast<uint32_t>(gpt->BlockSize()), std::move(gpt)));
}

zx::result<GptDevicePartitioner::InitializeGptResult> GptDevicePartitioner::InitializeGpt(
    const paver::BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
    fidl::ClientEnd<fuchsia_device::Controller> block_controller) {
  if (block_controller) {
    return InitializeProvidedGptDevice(devices, svc_root, block_controller);
  }

  zx::result admin = component::ConnectAt<fuchsia_fshost::Admin>(svc_root);
  if (admin.is_error()) {
    return admin.take_error();
  }
  fidl::WireResult storage_host = fidl::WireCall(*admin)->StorageHostEnabled();
  if (!storage_host.ok()) {
    ERROR("Failed to query fshost for storage-host: %s\n",
          storage_host.FormatDescription().c_str());
  }

  std::optional<paver::BlockDevices> gpt_device_source;
  std::unique_ptr<GptDevice> gpt;
  uint32_t block_size;
  uint64_t block_count;
  if (storage_host->enabled) {
    // Fshost takes care of finding the GPT block device.
    zx::result result = BlockDevices::CreateStorageHost(svc_root);
    if (result.is_error()) {
      ERROR("Failed to connect to storage host: %s\n", result.status_string());
      return result.take_error();
    }
    gpt_device_source = std::move(*result);

    zx::result manager = component::ConnectAt<fuchsia_storagehost::PartitionsManager>(svc_root);
    if (manager.is_error()) {
      return manager.take_error();
    }
    fidl::WireResult info = fidl::WireCall(*manager)->GetBlockInfo();
    if (!info.ok()) {
      ERROR("Warning: Could not acquire GPT block info: %s\n", info.FormatDescription().c_str());
      return zx::error(info.status());
    }
    if (info.value().is_error()) {
      ERROR("Warning: Could not acquire GPT block info: %s\n",
            zx_status_get_string((info.value().error_value())));
      return info.value().take_error();
    }
    block_count = info.value()->block_count;
    block_size = info.value()->block_size;
  } else {
    // In the legacy configuration, we try to find the block device ourselves.
    gpt_device_source = devices.Duplicate();

    zx::result gpt_devices = FindGptDevices(devices.devfs_root());
    if (gpt_devices.is_error()) {
      ERROR("Failed to find GPT: %s\n", gpt_devices.status_string());
      return gpt_devices.take_error();
    }

    std::vector<fidl::ClientEnd<fuchsia_device::Controller>> non_removable_gpt_devices;

    for (auto& gpt_device : gpt_devices.value()) {
      fidl::WireResult info = fidl::WireCall(gpt_device.block)->GetInfo();
      if (!info.ok()) {
        ERROR("Warning: Could not acquire GPT block info: %s\n", info.FormatDescription().c_str());
        return zx::error(info.status());
      }
      if (info.value().is_error()) {
        ERROR("Warning: Could not acquire GPT block info: %s\n",
              zx_status_get_string(info.value().error_value()));
        return info.value().take_error();
      }
      if (info.value()->info.flags & fuchsia_hardware_block::wire::Flag::kRemovable) {
        continue;
      }

      auto [controller, controller_server] = fidl::Endpoints<fuchsia_device::Controller>::Create();
      if (fidl::OneWayStatus status = fidl::WireCall(gpt_device.controller)
                                          ->ConnectToController(std::move(controller_server));
          !status.ok()) {
        ERROR("Failed to connect to new controller %s\n", status.FormatDescription().c_str());
        continue;
      }
      zx::result result = ConnectToGpt(std::move(controller));
      if (result.is_error()) {
        ERROR("Failed to connect to GPT: %s\n", result.status_string());
        return zx::error(ZX_ERR_BAD_STATE);
      }
      if (!result->Valid()) {
        continue;
      }

      if (gpt) {
        ERROR("Found multiple block devices with valid GPTs. Unsuppported.\n");
        return zx::error(ZX_ERR_NOT_SUPPORTED);
      }
      gpt = std::move(result.value());
      block_size = info->value()->info.block_size;
      block_count = info->value()->info.block_count;
    }
    if (!gpt) {
      ERROR("No candidate GPTs found.\n");
      return zx::error(ZX_ERR_NOT_FOUND);
    }
  }

  auto partitioner = WrapUnique(new GptDevicePartitioner(std::move(*gpt_device_source), svc_root,
                                                         block_count, block_size, std::move(gpt)));

  if (partitioner->FindPartition(IsFvmOrAndroidPartition).is_error()) {
    ERROR(
        "Unable to find a GPT on this device with the expected partitions. "
        "Please run fx init-partition-tables to re-initialize the device.\n");
  }

  return zx::ok(std::move(partitioner));
}

struct PartitionPosition {
  size_t start;   // Block, inclusive
  size_t length;  // In Blocks
};

zx::result<std::unique_ptr<BlockPartitionClient>> GptDevicePartitioner::FindPartition(
    FilterCallback filter) const {
  if (!StorageHostDetected()) {
    zx::result result = FindPartitionLegacy(std::move(filter));
    if (result.is_error()) {
      return result.take_error();
    }
    return zx::ok(std::move(result->partition));
  }
  zx::result result = devices_.OpenPartition([&](const zx::channel& chan) -> bool {
    auto client =
        fidl::UnownedClientEnd<fuchsia_hardware_block_partition::Partition>(chan.borrow());
    zx::result metadata = QueryGptPartitionMetadata(client);
    if (metadata.is_error()) {
      ERROR("Failed to query GPT partition metadata: %s\n", metadata.status_string());
      return false;
    }
    return filter(*metadata);
  });
  if (result.is_error()) {
    return result.take_error();
  }
  return BlockPartitionClient::Create(std::move(*result));
}

zx::result<std::vector<std::unique_ptr<BlockPartitionClient>>>
GptDevicePartitioner::FindAllPartitions(GptDevicePartitioner::FilterCallback filter) const {
  zx::result result = devices_.OpenAllPartitions([&](const zx::channel& chan) -> bool {
    auto client =
        fidl::UnownedClientEnd<fuchsia_hardware_block_partition::Partition>((chan.borrow()));
    zx::result metadata = QueryGptPartitionMetadata(client);
    if (metadata.is_error()) {
      if (metadata.status_value() != ZX_ERR_NOT_SUPPORTED) {
        ERROR("Failed to query GPT partition metadata: %s\n", metadata.status_string());
      }
      return false;
    }
    return filter(*metadata);
  });
  if (result.is_error()) {
    return result.take_error();
  }
  std::vector<std::unique_ptr<BlockPartitionClient>> clients;
  for (auto& connector : *result) {
    zx::result result = BlockPartitionClient::Create(std::move((connector)));
    if (result.is_error()) {
      return result.take_error();
    }
    clients.push_back(std::move(*result));
  }
  return zx::ok(std::move(clients));
}

zx::result<GptDevicePartitioner::FindPartitionDetailsResult>
GptDevicePartitioner::FindPartitionDetails(FilterCallback filter) const {
  if (StorageHostDetected()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  return FindPartitionLegacy(std::move(filter));
}

zx::result<GptDevicePartitioner::FindPartitionDetailsResult>
GptDevicePartitioner::FindPartitionLegacy(FilterCallback filter) const {
  for (uint32_t i = 0; i < gpt::kPartitionCount; i++) {
    zx::result<gpt_partition_t*> p = gpt_->GetPartition(i);
    if (p.is_error()) {
      continue;
    }
    GptPartitionMetadata metadata{};
    char name[(GPT_NAME_LEN / 2) + 1] = {'\0'};
    paver::utf16_to_cstring(name, p->name, GPT_NAME_LEN);
    metadata.name = std::string(name, strlen(name));
    metadata.instance_guid = Uuid((*p)->guid);
    metadata.type_guid = Uuid((*p)->type);

    if (filter(metadata)) {
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
      return zx::ok(FindPartitionDetailsResult{std::move(*part), *p});
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<> GptDevicePartitioner::WipeFvm() const {
  return WipeBlockPartition(devices_, std::nullopt, Uuid(GUID_FVM_VALUE));
}

zx::result<> GptDevicePartitioner::ResetPartitionTables(
    std::vector<GptDevicePartitioner::PartitionInitSpec> partitions) const {
  // Assign offsets and instance GUIDs as needed.
  uint64_t metadata_blocks = ReservedHeaderBlocks(block_size_);
  uint64_t last_available_block = block_count_ - metadata_blocks;
  struct Range {
    uint64_t start;
    uint64_t end;
  };
  std::vector<Range> allocations = {
      Range{.start = 0, .end = metadata_blocks},
      Range{.start = last_available_block, .end = block_count_},
  };

  // Returns the position to insert at, and the block offset to use.
  auto find_first_fit = [&](uint64_t num_blocks) -> zx::result<std::tuple<size_t, uint64_t>> {
    for (size_t i = 1; i < allocations.size(); ++i) {
      const auto& prev = allocations[i - 1];
      const auto& next = allocations[i];
      if (next.start - prev.end >= num_blocks) {
        return zx::ok(std::make_tuple(i, prev.end));
      }
    }
    return zx::error(ZX_ERR_NO_SPACE);
  };

  for (auto& partition : partitions) {
    if (partition.size_bytes == 0) {
      continue;
    }
    if (partition.size_bytes % block_size_ > 0) {
      ERROR("Misaligned partition\n");
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    uint64_t num_blocks = partition.size_bytes / block_size_;
    constexpr const Uuid kNilGuid;
    if (partition.instance == kNilGuid) {
      partition.instance = Uuid::Generate();
    }
    auto pos = allocations.end();
    if (partition.start_block == 0) {
      zx::result result = find_first_fit(num_blocks);
      if (result.is_error()) {
        return result.take_error();
      }
      auto [index, off] = *result;
      partition.start_block = off;
      pos = std::next(allocations.begin(), static_cast<int64_t>(index));
      LOG("Allocated partition %s @ %" PRIu64 "\n", partition.name.c_str(), off);
    } else {
      pos = std::lower_bound(
          allocations.begin(), allocations.end(), partition.start_block,
          [](const Range& range, uint64_t offset) -> bool { return range.start < offset; });
      if (pos == allocations.end()) {
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
    allocations.insert(pos, Range{
                                .start = partition.start_block,
                                .end = partition.start_block + num_blocks,
                            });
  }

  // Check to see if we're using storage-host.  If not, we'll fall back to writing the GPT
  // manually.
  // TODO(https://fxbug.dev/339491886): Remove fallback once products are using storage-host.
  if (!StorageHostDetected()) {
    LOG("Legacy mode; manually overwriting the GPT...\n");
    return ResetPartitionTablesLegacy(std::move(partitions));
  }

  std::vector<fuchsia_storagehost::wire::PartitionInfo> infos{
      partitions.size(), fuchsia_storagehost::wire::PartitionInfo{}};
  for (size_t i = 0; i < partitions.size(); ++i) {
    const auto& partition = partitions[i];
    if (partition.size_bytes == 0) {
      continue;
    }
    fuchsia_storagehost::wire::PartitionInfo info{
        .name = fidl::StringView::FromExternal(partition.name),
        .start_block = partition.start_block,
        .num_blocks = partition.size_bytes / block_size_,
        .flags = partition.flags,
    };
    std::copy(partition.type.cbegin(), partition.type.cend(), info.type_guid.value.data());
    std::copy(partition.instance.cbegin(), partition.instance.cend(),
              info.instance_guid.value.data());
    infos[i] = info;
  }

  zx::result recovery = component::ConnectAt<fuchsia_fshost::Recovery>(svc_root_.borrow());
  if (recovery.is_error()) {
    return recovery.take_error();
  }
  fidl::WireResult result = fidl::WireCall(*recovery)->InitSystemPartitionTable(
      fidl::VectorView<fuchsia_storagehost::wire::PartitionInfo>::FromExternal(infos));
  if (result.status() != ZX_OK) {
    ERROR("Failed to reset partitions table: %s\n", result.FormatDescription().c_str());
    return zx::error(result.status());
  }
  return zx::ok();
}

zx::result<> GptDevicePartitioner::ResetPartitionTablesLegacy(
    std::span<const PartitionInitSpec> partitions) const {
  if (zx_status_t status = gpt_->RemoveAllPartitions(); status != ZX_OK) {
    ERROR("Failed to remove GPT partitions: %s\n", zx_status_get_string(status));
    return zx::error(status);
  }
  for (const auto& partition : partitions) {
    if (partition.size_bytes == 0) {
      continue;
    }
    zx::result result = gpt_->AddPartition(partition.name.c_str(), partition.type.bytes(),
                                           partition.instance.bytes(), partition.start_block,
                                           partition.size_bytes / block_size_, partition.flags);
    if (result.is_error()) {
      ERROR("Failed to add partition %s at off %" PRIu64 ":  %s\n", partition.name.c_str(),
            partition.start_block, result.status_string());
      return result.take_error();
    }
  }
  if (zx_status_t status = gpt_->Sync(); status != ZX_OK) {
    return zx::error(status);
  }
  zx::result result = RebindGptDriver(svc_root_, gpt_->device());
  if (result.is_error()) {
    ERROR("Failed to rebind GPT driver: %s\n", result.status_string());
  }
  return result;
}

}  // namespace paver
