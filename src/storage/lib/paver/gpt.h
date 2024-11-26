// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_STORAGE_LIB_PAVER_GPT_H_
#define SRC_STORAGE_LIB_PAVER_GPT_H_

#include <fidl/fuchsia.storagehost/cpp/wire.h>
#include <lib/component/incoming/cpp/clone.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fit/function.h>
#include <lib/zx/channel.h>

#include <span>
#include <string_view>

#include <gpt/gpt.h>

#include "src/lib/uuid/uuid.h"
#include "src/storage/lib/paver/block-devices.h"
#include "src/storage/lib/paver/device-partitioner.h"

namespace paver {

using gpt::GptDevice;

// Android specific partition.
// Paver would use it's presence to detect system that has Android configuration.
constexpr std::string_view kGptSuperName = "super";

// Used as a search key for `GptDevicePartitioner.FindPartition`.
struct GptPartitionMetadata {
  std::string name;
  uuid::Uuid type_guid;
  uuid::Uuid instance_guid;
};

// Useful for when a GPT table is available (e.g. x86 devices). Provides common
// utility functions.
class GptDevicePartitioner {
 public:
  using FilterCallback = fit::function<bool(const GptPartitionMetadata&)>;

  struct InitializeGptResult {
    std::unique_ptr<GptDevicePartitioner> gpt;
    bool initialize_partition_tables;
  };

  // Find and initialize a GPT based device.
  //
  // If block_controller is provided, then that device is used directly.  Otherwise, query fshost
  // for the system GPT, and use that.
  //
  // If the system GPT was not formatted correctly, attempts to format it by calling
  // fshost.Admin.InitGpt.  This won't be done on directly provided `block_controller` instances.
  static zx::result<InitializeGptResult> InitializeGpt(
      const paver::BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      fidl::ClientEnd<fuchsia_device::Controller> block_controller);

  GptDevice* GetGpt() const { return gpt_.get(); }

  // Returns a connection to the first matching partition.
  zx::result<std::unique_ptr<BlockPartitionClient>> FindPartition(FilterCallback filter) const;

  // Returns a connection to all matching partitions.
  zx::result<std::vector<std::unique_ptr<BlockPartitionClient>>> FindAllPartitions(
      FilterCallback filter) const;

  struct FindPartitionDetailsResult {
    std::unique_ptr<BlockPartitionClient> partition;
    gpt_partition_t* gpt_partition;
  };

  // Returns a connection to the first matching partition, as well as its GPT metadata.
  // TODO(https://fxbug.dev/339491886): Remove once all products use storage-host.
  zx::result<FindPartitionDetailsResult> FindPartitionDetails(FilterCallback filter) const;

  // Wipes a specified partition from the GPT, and overwrites first 8KiB with
  // nonsense.
  zx::result<> WipeFvm() const;

  struct PartitionInitSpec {
   public:
    std::string name;
    uuid::Uuid type;
    // If zero, a random GUID will be assigned
    uuid::Uuid instance;
    // If nonzero, the partition will be allocated at the specific offset (and initialization will
    // fail if this overlaps with other partitions or the GPT itself).  If the value is zero, the
    // partition is dynamically allocated.  Dynamically allocated partitions must precede all
    // fixed-offset partitions in the list passed to ResetPartitionTables, otherwise they might be
    // allocated over the desired range.
    uint64_t start_block = 0;
    // Zero indicates an empty partition table entry; other fields are ignored.
    uint64_t size_bytes = 0;
    uint64_t flags = 0;

    static PartitionInitSpec ForKnownPartition(Partition partition, PartitionScheme scheme,
                                               size_t size_bytes);
  };

  // Wipes the partition table and resets it to `partitions`.
  // See fuchsia.storagehost.PartitionsManager/ResetPartitionTables.
  zx::result<> ResetPartitionTables(std::vector<PartitionInitSpec> partitions) const;

  const paver::BlockDevices& devices() { return devices_; }

  fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root() { return svc_root_; }

  // FIDL clients for a block device that could contain a GPT.
  struct GptClients {
    std::string topological_path;
    fidl::ClientEnd<fuchsia_hardware_block::Block> block;
    fidl::ClientEnd<fuchsia_device::Controller> controller;
  };

  // Find all block devices which could contain a GPT.
  // TODO(https://fxbug.dev/339491886): Remove this and migrate users to interact with the
  // fuchsia.paver.Paver (or fuchsia.fshost.Admin directly) to connect to and interact with the GPT.
  static zx::result<std::vector<GptClients>> FindGptDevices(const fbl::unique_fd& devfs_root);

 private:
  // Initializes GPT for a device which was explicitly provided. If |gpt_device| doesn't have a
  // valid GPT, it will initialize it with a valid one.
  static zx::result<std::unique_ptr<GptDevicePartitioner>> InitializeProvidedGptDevice(
      const paver::BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      fidl::UnownedClientEnd<fuchsia_device::Controller> gpt_device);

  GptDevicePartitioner(BlockDevices devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
                       uint64_t block_count, uint32_t block_size, std::unique_ptr<GptDevice> gpt)
      : devices_(std::move(devices)),
        block_count_(block_count),
        block_size_(block_size),
        svc_root_(component::MaybeClone(svc_root)),
        gpt_(std::move(gpt)) {
    // gpt_ is only set for the legacy Devfs configuration.
    ZX_DEBUG_ASSERT(devices_.IsStorageHost() ^ gpt_ != nullptr);
  }

  // Detects if the system has storage-host.
  // If not, the below Legacy methods will substitute their matching method.
  bool StorageHostDetected() const { return devices_.IsStorageHost(); }

  // Returns a file descriptor to a partition which can be paved, if one exists.
  // TODO(https://fxbug.dev/339491886): Remove once products are using storage-host.
  zx::result<FindPartitionDetailsResult> FindPartitionLegacy(FilterCallback filter) const;

  // Reset the partition table by directly overwriting the GPT.
  // TODO(https://fxbug.dev/339491886): Remove once products are using storage-host.
  zx::result<> ResetPartitionTablesLegacy(std::span<const PartitionInitSpec> partitions) const;

  const paver::BlockDevices devices_;
  const uint64_t block_count_;
  const uint64_t block_size_;
  fidl::ClientEnd<fuchsia_io::Directory> svc_root_;
  // When storage-host is disabled, this can be used to read from the GPT device.
  // TODO(https://fxbug.dev/339491886): Remove once products are using storage-host.
  mutable std::unique_ptr<GptDevice> gpt_;
};

// TODO(69527): Remove this and migrate usages to |utf16_to_utf8|
inline void utf16_to_cstring(char* dst, const uint8_t* src, size_t charcount) {
  while (charcount > 0) {
    *dst++ = *src;
    src += 2;
    charcount -= 2;
  }
}

inline bool FilterByType(const GptPartitionMetadata& part, const uuid::Uuid& type) {
  return type == part.type_guid;
}

bool FilterByName(const GptPartitionMetadata& part, std::string_view name);

bool FilterByTypeAndName(const GptPartitionMetadata& part, const uuid::Uuid& type,
                         std::string_view name);

inline bool IsFvmPartition(const GptPartitionMetadata& part) {
  return FilterByType(part, GUID_FVM_VALUE) ||
         FilterByTypeAndName(part, GPT_FVM_TYPE_GUID, GPT_FVM_NAME);
}

inline bool IsAndroidPartition(const GptPartitionMetadata& part) {
  // Check for Android specific partition 'super'.
  return FilterByName(part, kGptSuperName);
}

inline bool IsFvmOrAndroidPartition(const GptPartitionMetadata& part) {
  return IsFvmPartition(part) || IsAndroidPartition(part);
}

// Returns true if the spec partition is Zircon A/B/R.
inline bool IsZirconPartitionSpec(const PartitionSpec& spec) {
  return spec.partition == Partition::kZirconA || spec.partition == Partition::kZirconB ||
         spec.partition == Partition::kZirconR;
}

}  // namespace paver

#endif  // SRC_STORAGE_LIB_PAVER_GPT_H_
