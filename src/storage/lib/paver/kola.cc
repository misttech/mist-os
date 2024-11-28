// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/kola.h"

#include <fidl/fuchsia.storagehost/cpp/wire_types.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fit/defer.h>
#include <lib/zx/result.h>

#include <algorithm>
#include <iterator>
#include <string>

#include <gpt/gpt.h>
#include <hwreg/bitfields.h>

#include "src/lib/uuid/uuid.h"
#include "src/storage/lib/paver/pave-logging.h"
#include "src/storage/lib/paver/system_shutdown_state.h"
#include "src/storage/lib/paver/utils.h"
#include "src/storage/lib/paver/validation.h"

namespace paver {
namespace {

using fuchsia_system_state::SystemPowerState;
using uuid::Uuid;

}  // namespace

zx::result<std::unique_ptr<DevicePartitioner>> KolaPartitioner::Initialize(
    const BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
    fidl::ClientEnd<fuchsia_device::Controller> block_device) {
  if (IsBoard(svc_root, "kola").is_error() && IsBoard(svc_root, "sorrel").is_error()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  auto gpt = GptDevicePartitioner::InitializeGpt(devices, svc_root, std::move(block_device));
  if (gpt.is_error()) {
    return gpt.take_error();
  }
  if (gpt->initialize_partition_tables) {
    LOG("Found GPT but it was missing expected partitions.  The device should be re-initialized "
        "via fastboot.\n");
    return zx::error(ZX_ERR_BAD_STATE);
  }

  auto partitioner = WrapUnique(new KolaPartitioner(std::move(gpt->gpt)));

  LOG("Successfully initialized Kola Device Partitioner\n");
  return zx::ok(std::move(partitioner));
}

const paver::BlockDevices& KolaPartitioner::Devices() const { return gpt_->devices(); }

fidl::UnownedClientEnd<fuchsia_io::Directory> KolaPartitioner::SvcRoot() const {
  return gpt_->svc_root();
}

bool KolaPartitioner::SupportsPartition(const PartitionSpec& spec) const {
  constexpr PartitionSpec supported_specs[] = {
      PartitionSpec(paver::Partition::kZirconA), PartitionSpec(paver::Partition::kZirconB),
      PartitionSpec(paver::Partition::kFuchsiaVolumeManager)};
  return std::any_of(std::cbegin(supported_specs), std::cend(supported_specs),
                     [&](const PartitionSpec& supported) { return SpecMatches(spec, supported); });
}

zx::result<std::string> KolaPartitioner::PartitionNameForSpec(const PartitionSpec& spec) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  std::string part_name;
  switch (spec.partition) {
    case Partition::kZirconA:
      part_name = "boot_a";
      break;
    case Partition::kZirconB:
      part_name = "boot_b";
      break;
    case Partition::kFuchsiaVolumeManager:
      part_name = "super";
      break;
    default:
      ERROR("Kola partitioner cannot find unknown partition type\n");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  return zx::ok(std::move(part_name));
}

zx::result<std::unique_ptr<PartitionClient>> KolaPartitioner::FindPartition(
    const PartitionSpec& spec) const {
  return FindGptPartition(spec);
}

zx::result<std::unique_ptr<BlockPartitionClient>> KolaPartitioner::FindGptPartition(
    const PartitionSpec& spec) const {
  zx::result name = PartitionNameForSpec(spec);
  if (name.is_error()) {
    return name.take_error();
  }
  return gpt_->FindPartition(
      [&](const GptPartitionMetadata& part) { return FilterByName(part, *name); });
}

zx::result<std::vector<std::unique_ptr<BlockPartitionClient>>> KolaPartitioner::FindAllPartitions(
    FilterCallback filter) const {
  return gpt_->FindAllPartitions(std::move(filter));
}

zx::result<FindPartitionDetailsResult> KolaPartitioner::FindPartitionDetails(
    const PartitionSpec& spec) const {
  zx::result name = PartitionNameForSpec(spec);
  if (name.is_error()) {
    return name.take_error();
  }
  return gpt_->FindPartitionDetails(
      [&](const GptPartitionMetadata& part) { return FilterByName(part, *name); });
}

zx::result<> KolaPartitioner::WipeFvm() const { return gpt_->WipeFvm(); }

zx::result<> KolaPartitioner::ResetPartitionTables() const {
  ERROR("Initialising partition tables is not supported for a Kola device\n");
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> KolaPartitioner::ValidatePayload(const PartitionSpec& spec,
                                              std::span<const uint8_t> data) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  if (IsZirconPartitionSpec(spec)) {
    if (!IsValidAndroidKernel(data)) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
  }

  return zx::ok();
}

zx::result<> KolaPartitioner::OnStop() const {
  const auto state = GetShutdownSystemState(gpt_->svc_root());
  switch (state) {
    case SystemPowerState::kRebootBootloader:
      ERROR("Setting one shot reboot to bootloader flag is not implemented.\n");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    case SystemPowerState::kRebootRecovery:
      ERROR("Setting one shot reboot to recovery flag is not implemented.\n");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    case SystemPowerState::kFullyOn:
    case SystemPowerState::kReboot:
    case SystemPowerState::kPoweroff:
    case SystemPowerState::kMexec:
    case SystemPowerState::kSuspendRam:
    case SystemPowerState::kRebootKernelInitiated:
      // nothing to do for these cases
      break;
  }

  return zx::ok();
}

zx::result<std::unique_ptr<DevicePartitioner>> KolaPartitionerFactory::New(
    const BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root, Arch arch,
    std::shared_ptr<Context> context, fidl::ClientEnd<fuchsia_device::Controller> block_device) {
  return KolaPartitioner::Initialize(devices, svc_root, std::move(block_device));
}

class KolaAbrManagerInterface {
 public:
  virtual ~KolaAbrManagerInterface() = default;
  // Gets the flags for slots A and B.  If there are pending changes to the flags, they are
  // included.
  virtual zx::result<> GetPartitionFlags(uint64_t* a_flags, uint64_t* b_flags) = 0;
  // Sets the flags for slots A and B.
  virtual zx::result<> SetPartitionFlags(uint64_t a_flags, uint64_t b_flags) = 0;
  // Swaps the type GUIDs of all A/B partitions.
  virtual zx::result<> SwapAbPartitionTypeGuids(bool new_slot_is_b) = 0;
  /// Discards all pending changes.  This must be called if any of the above methods fail.
  virtual void Discard() = 0;
  virtual zx::result<> Commit() = 0;
};

/// Implementation of A/B management which relies on APIs offered by storage-host.
class KolaAbrManager : public KolaAbrManagerInterface {
 public:
  static zx::result<std::unique_ptr<KolaAbrManager>> Create(const KolaPartitioner* partitioner) {
    zx::result zircon_a = partitioner->FindGptPartition(PartitionSpec(Partition::kZirconA));
    if (zircon_a.is_error()) {
      ERROR("Failed to find Zircon A partition\n");
      return zircon_a.take_error();
    }

    zx::result zircon_b = partitioner->FindGptPartition(PartitionSpec(Partition::kZirconB));
    if (zircon_b.is_error()) {
      ERROR("Failed to find Zircon B partition\n");
      return zircon_b.take_error();
    }

    auto [client, server] = fidl::Endpoints<fuchsia_storagehost::PartitionsManager>::Create();
    zx::result result = component::ConnectAt(partitioner->SvcRoot(), std::move(server));
    if (result.is_error()) {
      return result.take_error();
    }

    return zx::ok(new KolaAbrManager(partitioner, std::move(zircon_a.value()),
                                     std::move(zircon_b.value()), std::move(client)));
  }

  zx::result<> GetPartitionFlags(uint64_t* a_flags, uint64_t* b_flags) override {
    if (pending_zircon_a_flags_) {
      *a_flags = *pending_zircon_a_flags_;
    } else {
      zx::result a = zircon_a_->GetMetadata();
      if (a.is_error()) {
        return a.take_error();
      }
      *a_flags = a->flags;
    }
    if (pending_zircon_b_flags_) {
      *b_flags = *pending_zircon_b_flags_;
    } else {
      zx::result b = zircon_b_->GetMetadata();
      if (b.is_error()) {
        return b.take_error();
      }
      *b_flags = b->flags;
    }
    return zx::ok();
  }

  zx::result<> SetPartitionFlags(uint64_t a_flags, uint64_t b_flags) override {
    zx::result result = UpdatePartitionMetadata(*zircon_a_, a_flags, {});
    if (result.is_error()) {
      return result.take_error();
    }
    result = UpdatePartitionMetadata(*zircon_b_, b_flags, {});
    if (result.is_error()) {
      return result.take_error();
    }
    pending_zircon_a_flags_ = a_flags;
    pending_zircon_b_flags_ = b_flags;
    return zx::ok();
  }

  zx::result<> SwapAbPartitionTypeGuids(bool new_slot_is_b) override {
    zx::result a_partitions = partitioner_->FindAllPartitions(
        [](const GptPartitionMetadata& metadata) -> bool { return metadata.name.ends_with("_a"); });
    if (a_partitions.is_error()) {
      ERROR("Failed to find a partitions:%s \n", a_partitions.status_string());
      return a_partitions.take_error();
    }
    zx::result b_partitions = partitioner_->FindAllPartitions(
        [](const GptPartitionMetadata& metadata) -> bool { return metadata.name.ends_with("_b"); });
    if (b_partitions.is_error()) {
      ERROR("Failed to find b partitions:%s \n", b_partitions.status_string());
      return b_partitions.take_error();
    }
    if (a_partitions->size() != b_partitions->size()) {
      ERROR("Unexpectedly found %zu a partitions and %zu b partitions\n", a_partitions->size(),
            b_partitions->size());
      return zx::error(ZX_ERR_BAD_STATE);
    }

    struct Partition {
      std::unique_ptr<BlockPartitionClient> client;
      PartitionMetadata metadata;
    };
    auto create_partition_map = [](std::vector<std::unique_ptr<BlockPartitionClient>> partitions)
        -> zx::result<std::unordered_map<std::string, Partition>> {
      std::unordered_map<std::string, Partition> partitions_map;
      for (auto& part : partitions) {
        zx::result metadata = part->GetMetadata();
        if (metadata.is_error()) {
          ERROR("Failed to get metadata: %s\n", metadata.status_string());
          return metadata.take_error();
        }
        ZX_DEBUG_ASSERT(metadata->name.size() >= 2);
        std::string base_name = metadata->name.substr(0, metadata->name.size() - 2);
        partitions_map[base_name] = Partition{
            .client = std::move(part),
            .metadata = std::move(*metadata),
        };
      }
      return zx::ok(std::move(partitions_map));
    };
    zx::result a_partitions_map = create_partition_map(std::move(*a_partitions));
    if (a_partitions_map.is_error()) {
      return a_partitions_map.take_error();
    }
    zx::result b_partitions_map = create_partition_map(std::move(*b_partitions));
    if (b_partitions_map.is_error()) {
      return b_partitions_map.take_error();
    }

    const std::unordered_map<std::string, Partition>& new_partitions =
        new_slot_is_b ? *b_partitions_map : *a_partitions_map;
    const std::unordered_map<std::string, Partition>& old_partitions =
        new_slot_is_b ? *a_partitions_map : *b_partitions_map;

    auto iter = new_partitions.find("boot");
    if (iter == new_partitions.end()) {
      ERROR("Failed to find the boot partition.\n");
      return zx::error(ZX_ERR_BAD_STATE);
    }
    const Uuid& inactive_type_guid = iter->second.metadata.type_guid;

    // Check that all of the new partitions have the same type GUID (inactive_type_guid) and have a
    // corresponding old partition, and then swap the type GUIDs.
    for (const auto& [part_name, new_part] : new_partitions) {
      if (new_part.metadata.type_guid != inactive_type_guid) {
        ERROR("Partition type GUID mismatch: %s partition has type %s (expected %s)\n",
              part_name.c_str(), new_part.metadata.type_guid.ToString().c_str(),
              inactive_type_guid.ToString().c_str());
        return zx::error(ZX_ERR_BAD_STATE);
      }
      auto old_part = old_partitions.find(part_name);
      if (old_part == old_partitions.end()) {
        ERROR("Failed to find corresponding %s partition.\n", part_name.c_str());
        return zx::error(ZX_ERR_BAD_STATE);
      }
      const Uuid& active_type_guid = old_part->second.metadata.type_guid;

      zx::result result = UpdatePartitionMetadata(*new_part.client, {}, active_type_guid);
      if (result.is_error()) {
        ERROR("Failed to update type GUID: %s\n", result.status_string());
        return result.take_error();
      }
      result = UpdatePartitionMetadata(*old_part->second.client, {}, inactive_type_guid);
      if (result.is_error()) {
        ERROR("Failed to update type GUID: %s\n", result.status_string());
        return result.take_error();
      }
    }
    return zx::ok();
  }

  void Discard() override {
    transaction_.reset();
    pending_zircon_a_flags_.reset();
    pending_zircon_b_flags_.reset();
  }

  zx::result<> Commit() override {
    if (transaction_.is_valid()) {
      fidl::WireResult result = partitions_manager_->CommitTransaction(std::move(transaction_));
      if (!result.ok()) {
        ERROR("Failed to commit transaction: %s\n", result.status_string());
        return zx::error(result.status());
      }
    }
    Discard();
    return zx::ok();
  }

 private:
  KolaAbrManager(const KolaPartitioner* partitioner, std::unique_ptr<BlockPartitionClient> zircon_a,
                 std::unique_ptr<BlockPartitionClient> zircon_b,
                 fidl::ClientEnd<fuchsia_storagehost::PartitionsManager> partitions_manager)
      : partitioner_(partitioner),
        zircon_a_(std::move(zircon_a)),
        zircon_b_(std::move(zircon_b)),
        partitions_manager_(std::move(partitions_manager)) {}

  zx::result<> UpdatePartitionMetadata(PartitionClient& client, std::optional<uint64_t> flags,
                                       std::optional<Uuid> type_guid) {
    zx::result partition = client.connector()->PartitionManagement();
    if (partition.is_error()) {
      return partition.take_error();
    }
    fidl::Arena arena;
    auto request = fuchsia_storagehost::wire::PartitionUpdateMetadataRequest::Builder(arena);
    zx::result transaction = GetTransactionToken();
    if (transaction.is_error()) {
      return transaction.take_error();
    }
    request.transaction(std::move(*transaction));
    if (flags) {
      request.flags(*flags);
    }
    fuchsia_hardware_block_partition::wire::Guid type;
    if (type_guid) {
      ZX_DEBUG_ASSERT(uuid::kUuidSize == type.value.size());
      memcpy(type.value.data(), type_guid->bytes(), uuid::kUuidSize);
      request.type_guid(type);
    }
    fidl::WireResult result =
        fidl::WireCall<fuchsia_storagehost::Partition>(*partition)->UpdateMetadata(request.Build());
    if (!result.ok()) {
      ERROR("Failed to update metadata: %s\n", result.status_string());
      return zx::error(result.status());
    }
    return zx::ok();
  }

  zx::result<zx::eventpair> GetTransactionToken() {
    if (!transaction_.is_valid()) {
      fidl::WireResult result = partitions_manager_->CreateTransaction();
      if (result->is_error()) {
        ERROR("Failed to create A/B transaction: %s\n", zx_status_get_string(result.status()));
        return zx::error(result.status());
      }
      transaction_ = std::move(result->value()->transaction);
    }
    zx::eventpair transaction;
    if (zx_status_t status = transaction_.duplicate(ZX_RIGHT_SAME_RIGHTS, &transaction);
        status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok(std::move(transaction));
  }

  const KolaPartitioner* partitioner_;
  std::unique_ptr<BlockPartitionClient> zircon_a_;
  std::unique_ptr<BlockPartitionClient> zircon_b_;
  fidl::WireSyncClient<fuchsia_storagehost::PartitionsManager> partitions_manager_;
  zx::eventpair transaction_;
  std::optional<uint64_t> pending_zircon_a_flags_;
  std::optional<uint64_t> pending_zircon_b_flags_;
};

/// Implementation of A/B management which relies on directly writing to the GPT.
/// TODO(https://fxbug.dev/339491886): Remove when products use storage-host.
class KolaLegacyAbrManager : public KolaAbrManagerInterface {
 public:
  static zx::result<std::unique_ptr<KolaLegacyAbrManager>> Create(
      const KolaPartitioner* partitioner) {
    zx::result<FindPartitionDetailsResult> zircon_a =
        partitioner->FindPartitionDetails(PartitionSpec(Partition::kZirconA));
    if (zircon_a.is_error()) {
      ERROR("Failed to find Zircon A partition\n");
      return zircon_a.take_error();
    }

    zx::result<FindPartitionDetailsResult> zircon_b =
        partitioner->FindPartitionDetails(PartitionSpec(Partition::kZirconB));
    if (zircon_b.is_error()) {
      ERROR("Failed to find Zircon B partition\n");
      return zircon_b.take_error();
    }

    return zx::ok(new KolaLegacyAbrManager(partitioner, zircon_a->index, zircon_b->index));
  }

  zx::result<> GetPartitionFlags(uint64_t* a_flags, uint64_t* b_flags) override {
    zx::result gpt = GetGpt();
    if (gpt.is_error()) {
      return gpt.take_error();
    }
    if (zx_status_t status = gpt->GetPartitionFlags(zircon_a_index_, a_flags); status != ZX_OK) {
      return zx::error(status);
    }
    if (zx_status_t status = gpt->GetPartitionFlags(zircon_b_index_, b_flags); status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok();
  }

  zx::result<> SetPartitionFlags(uint64_t a_flags, uint64_t b_flags) override {
    zx::result gpt = GetGpt();
    if (gpt.is_error()) {
      return gpt.take_error();
    }
    if (zx_status_t status = gpt->SetPartitionFlags(zircon_a_index_, a_flags); status != ZX_OK) {
      return zx::error(status);
    }
    if (zx_status_t status = gpt->SetPartitionFlags(zircon_b_index_, b_flags); status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok();
  }

  zx::result<> SwapAbPartitionTypeGuids(bool new_slot_is_b) override {
    // Find all slot-specific partitions.
    std::unordered_map<std::string, gpt_partition_t*> a_partitions;
    std::unordered_map<std::string, gpt_partition_t*> b_partitions;
    zx::result gpt = GetGpt();
    if (gpt.is_error()) {
      return gpt.take_error();
    }
    for (uint32_t i = 0; i < gpt::kPartitionCount; i++) {
      zx::result<gpt_partition_t*> p = gpt->GetPartition(i);
      if (p.is_error()) {
        continue;
      }
      gpt_partition_t* gpt_entry = p.value();

      char cstring_name[(GPT_NAME_LEN / 2) + 1] = {0};
      ::utf16_to_cstring(cstring_name, reinterpret_cast<const uint16_t*>(gpt_entry->name),
                         sizeof(cstring_name));
      const std::string_view name = cstring_name;
      if (name.length() < 2) {
        continue;
      }

      const std::string_view name_without_suffix = name.substr(0, name.length() - 2);
      if (name.ends_with("_a")) {
        a_partitions[std::string(name_without_suffix)] = gpt_entry;  // Drop "_a" suffix.
      } else if (name.ends_with("_b")) {
        b_partitions[std::string(name_without_suffix)] = gpt_entry;  // Drop "_b" suffix.
      }
    }
    if (a_partitions.size() != b_partitions.size()) {
      ERROR("Different number of slot A partitions and slot B partitions.\n");
      return zx::error(ZX_ERR_BAD_STATE);
    }

    // Check that all of the new partitions have the same type GUID and have a corresponding old
    // partition.
    const std::unordered_map<std::string, gpt_partition_t*>& new_partitions =
        new_slot_is_b ? b_partitions : a_partitions;
    const std::unordered_map<std::string, gpt_partition_t*>& old_partitions =
        new_slot_is_b ? a_partitions : b_partitions;
    auto iter = new_partitions.find("boot");
    if (iter == new_partitions.end()) {
      ERROR("Failed to find the boot partition.\n");
      return zx::error(ZX_ERR_BAD_STATE);
    }
    uint8_t type_guid[GPT_GUID_LEN];
    memcpy(type_guid, iter->second->type, GPT_GUID_LEN);
    bool abort = false;
    for (const auto& [part_name, part_entry] : new_partitions) {
      if (memcmp(part_entry->type, type_guid, GPT_GUID_LEN) != 0) {
        ERROR("Partition type GUID mismatch: %s partition has type %s (expected %s)\n",
              part_name.c_str(), Uuid(part_entry->type).ToString().c_str(),
              Uuid(type_guid).ToString().c_str());
        abort = true;
      }
      auto iter = old_partitions.find(part_name);
      if (iter == old_partitions.end()) {
        ERROR("Failed to find corresponding %s partition.\n", part_name.c_str());
        abort = true;
      }
    }
    if (abort) {
      ERROR("Aborting A/B partition type GUID swap.\n");
      return zx::error(ZX_ERR_BAD_STATE);
    }

    // Swap the A/B partition type GUIDs.
    for (const auto& [a_part_name, a_part_entry] : a_partitions) {
      gpt_partition_t* b_part_entry = b_partitions[a_part_name];
      memcpy(type_guid, a_part_entry->type, GPT_GUID_LEN);
      memcpy(a_part_entry->type, b_part_entry->type, GPT_GUID_LEN);
      memcpy(b_part_entry->type, type_guid, GPT_GUID_LEN);
    }
    return zx::ok();
  }

  void Discard() override { gpt_.reset(); }

  zx::result<> Commit() override {
    zx::result<> result = zx::ok();
    if (gpt_) {
      result = zx::make_result(gpt_->Sync());
    }
    gpt_.reset();
    return result;
  }

 private:
  KolaLegacyAbrManager(const KolaPartitioner* partitioner, uint32_t zircon_a_index,
                       uint32_t zircon_b_index)
      : partitioner_(partitioner),
        zircon_a_index_(zircon_a_index),
        zircon_b_index_(zircon_b_index) {}

  zx::result<GptDevice*> GetGpt() {
    if (!gpt_) {
      zx::result gpt = partitioner_->ConnectToGpt();
      if (gpt.is_error()) {
        return gpt.take_error();
      }
      gpt_ = std::move(*gpt);
    }
    return zx::ok(gpt_.get());
  }

  const KolaPartitioner* partitioner_;
  std::unique_ptr<GptDevice> gpt_;
  uint32_t zircon_a_index_;
  uint32_t zircon_b_index_;
};

/// Implementation of A/B management which relies on APIs offered by storage-host.
class KolaAbrClient : public abr::Client {
 public:
  explicit KolaAbrClient(std::unique_ptr<KolaAbrManagerInterface> abr)
      : Client(/*custom=*/true), abr_(std::move(abr)) {}

  zx::result<> Flush() override { return abr_->Commit(); }

 private:
  zx::result<> Read(uint8_t* buffer, size_t size) override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zx::result<> Write(const uint8_t* buffer, size_t size) override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zx::result<> ReadCustom(AbrSlotData* a, AbrSlotData* b, uint8_t* one_shot_recovery) override {
    uint64_t a_flags, b_flags;
    zx::result result = abr_->GetPartitionFlags(&a_flags, &b_flags);
    if (result.is_error()) {
      return result.take_error();
    }
    *a = ToFuchsia(a_flags);
    *b = ToFuchsia(b_flags);

    // TODO(b/348034903): Consider checking that the higher-priority active slot has the active
    // partition type GUIDs.

    *one_shot_recovery = 0;  // not supported
    return zx::ok();
  }

  zx::result<> WriteCustom(const AbrSlotData* a, const AbrSlotData* b,
                           uint8_t one_shot_recovery) override {
    bool slot_switch = DetectSlotSwitch(a, b);
    bool new_slot_is_b = false;
    if (slot_switch) {
      new_slot_is_b = true;
    } else {
      slot_switch = DetectSlotSwitch(b, a);
    }

    uint64_t a_flags, b_flags;
    zx::result result = abr_->GetPartitionFlags(&a_flags, &b_flags);
    if (result.is_error()) {
      return result.take_error();
    }
    a_flags = ToKola(*a, *b, a_flags);
    b_flags = ToKola(*b, *a, b_flags);

    auto discard_changes = fit::defer([&]() { abr_->Discard(); });

    // SetActiveAndUnbootable calls back into ReadCustom to read the flags, so we need to set them
    // here, even though we'll update them and call SetPartitionFlags again after.
    result = abr_->SetPartitionFlags(a_flags, b_flags);
    if (result.is_error()) {
      return result.take_error();
    }
    zx::result<uint64_t> new_a_flags = SetActiveAndUnbootable(kAbrSlotIndexA, a_flags);
    if (new_a_flags.is_error()) {
      return new_a_flags.take_error();
    }
    zx::result<uint64_t> new_b_flags = SetActiveAndUnbootable(kAbrSlotIndexB, b_flags);
    if (new_b_flags.is_error()) {
      return new_b_flags.take_error();
    }

    result = abr_->SetPartitionFlags(*new_a_flags, *new_b_flags);
    if (result.is_error()) {
      return result.take_error();
    }

    if (slot_switch) {
      zx::result result = abr_->SwapAbPartitionTypeGuids(new_slot_is_b);
      if (result.is_error()) {
        return result.take_error();
      }
      LOG("Switching active slot from %s\n", new_slot_is_b ? "A to B" : "B to A");
    }
    discard_changes.cancel();
    return zx::ok();
  }

  struct GptEntryAttributes {
    static constexpr uint8_t kKolaMaxPriority = 3;

    explicit GptEntryAttributes(uint64_t flags) : flags(flags) {}

    uint64_t flags;
    DEF_SUBFIELD(flags, 49, 48, priority);
    DEF_SUBBIT(flags, 50, active);
    DEF_SUBFIELD(flags, 53, 51, retry_count);
    DEF_SUBBIT(flags, 54, boot_success);
    DEF_SUBBIT(flags, 55, unbootable);
  };

  static bool DetectSlotSwitch(const AbrSlotData* old_slot, const AbrSlotData* new_slot) {
    return new_slot->priority == kAbrMaxPriority &&
           new_slot->tries_remaining == kAbrMaxTriesRemaining && !new_slot->successful_boot &&
           old_slot->priority < kAbrMaxPriority && old_slot->successful_boot;
  }

  static uint64_t ToKola(const AbrSlotData& current, const AbrSlotData& alternative,
                         uint64_t gpt_entry_flags) {
    // The priority field in Kola is only 2 bits wide (max value 3). Normalize AbrSlotData::priority
    // while maintaining the slots' relative priority.
    const uint8_t kola_priority = current.priority >= alternative.priority
                                      ? GptEntryAttributes::kKolaMaxPriority
                                      : GptEntryAttributes::kKolaMaxPriority - 1;

    GptEntryAttributes attributes(gpt_entry_flags);
    attributes.set_priority(kola_priority)
        .set_retry_count(current.tries_remaining)  // Both fields are 3 bits wide.
        .set_boot_success(current.successful_boot);
    return attributes.flags;
  }

  zx::result<uint64_t> SetActiveAndUnbootable(AbrSlotIndex slot_index, uint64_t gpt_entry_flags) {
    // Use GetSlotInfo's logic of determining is_active/is_bootable.
    zx::result<AbrSlotInfo> slot_info = GetSlotInfo(slot_index);
    if (slot_info.is_error()) {
      ERROR("Failed to get info for slot %s: %s\n",
            slot_index == kAbrSlotIndexA ? "A" : (slot_index == kAbrSlotIndexB ? "B" : "R"),
            slot_info.status_string());
      return slot_info.take_error();
    }

    GptEntryAttributes attributes(gpt_entry_flags);
    attributes.set_active(slot_info->is_active).set_unbootable(!slot_info->is_bootable);
    return zx::ok(attributes.flags);
  }

  static AbrSlotData ToFuchsia(uint64_t gpt_entry_flags) {
    const GptEntryAttributes attributes(gpt_entry_flags);

    AbrSlotData abr_slot_data = {};
    abr_slot_data.priority = static_cast<uint8_t>(attributes.priority());
    // Both fields are 3 bits wide.
    abr_slot_data.tries_remaining = static_cast<uint8_t>(attributes.retry_count());
    abr_slot_data.successful_boot = static_cast<uint8_t>(attributes.boot_success());
    return abr_slot_data;
  }

  std::unique_ptr<KolaAbrManagerInterface> abr_;
};

zx::result<std::unique_ptr<abr::Client>> KolaPartitioner::CreateAbrClient() const {
  std::unique_ptr<KolaAbrManagerInterface> abr;
  if (gpt_->devices().IsStorageHost()) {
    zx::result result = KolaAbrManager::Create(this);
    if (result.is_error()) {
      ERROR("Failed to create ABR manager: %s\n", result.status_string());
      return result.take_error();
    }
    abr = std::move(*result);
  } else {
    zx::result result = KolaLegacyAbrManager::Create(this);
    if (result.is_error()) {
      ERROR("Failed to create ABR manager: %s\n", result.status_string());
      return result.take_error();
    }
    abr = std::move(*result);
  }
  return zx::ok(std::make_unique<KolaAbrClient>(std::move(abr)));
}

}  // namespace paver
