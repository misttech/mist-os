// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/kola.h"

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

zx::result<std::unique_ptr<PartitionClient>> KolaPartitioner::FindPartition(
    const PartitionSpec& spec) const {
  zx::result<FindPartitionDetailsResult> result = FindPartitionDetails(spec);
  if (result.is_error()) {
    return result.take_error();
  }

  return zx::ok(std::move(result.value().partition));
}

zx::result<std::vector<std::unique_ptr<BlockPartitionClient>>> KolaPartitioner::FindAllPartitions(
    FilterCallback filter) const {
  return gpt_->FindAllPartitions(std::move(filter));
}

zx::result<FindPartitionDetailsResult> KolaPartitioner::FindPartitionDetails(
    const PartitionSpec& spec) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  std::string_view part_name;
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

  const auto filter = [&](const GptPartitionMetadata& part) {
    return FilterByName(part, part_name);
  };
  auto status = gpt_->FindPartitionDetails(filter);
  if (status.is_error()) {
    return status.take_error();
  }

  return zx::ok(std::move(status.value()));
}

zx::result<> KolaPartitioner::FinalizePartition(const PartitionSpec& spec) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  return zx::make_result(gpt_->GetGpt()->Sync());
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

class KolaAbrClient : public abr::Client {
 public:
  static zx::result<std::unique_ptr<abr::Client>> Create(const KolaPartitioner* partitioner) {
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

    return zx::ok(
        new KolaAbrClient(partitioner, std::move(zircon_a.value()), std::move(zircon_b.value())));
  }

 private:
  KolaAbrClient(const KolaPartitioner* partitioner, FindPartitionDetailsResult zircon_a,
                FindPartitionDetailsResult zircon_b)
      : Client(/*custom=*/true),
        partitioner_(partitioner),
        zircon_a_(std::move(zircon_a)),
        zircon_b_(std::move(zircon_b)) {}

  zx::result<> Read(uint8_t* buffer, size_t size) override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zx::result<> Write(const uint8_t* buffer, size_t size) override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  uint64_t ToKola(const AbrSlotData& current, const AbrSlotData& alternative,
                  uint64_t gpt_entry_flags) {
    // The priority field in Kola is only 2 bits wide (max value 3). Normalize AbrSlotData::priority
    // while maintaining the slots' relative priority.
    const uint8_t kola_priority = current.priority >= alternative.priority
                                      ? KolaGptEntryAttributes::kKolaMaxPriority
                                      : KolaGptEntryAttributes::kKolaMaxPriority - 1;

    KolaGptEntryAttributes attributes(gpt_entry_flags);
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

    KolaGptEntryAttributes attributes(gpt_entry_flags);
    attributes.set_active(slot_info->is_active).set_unbootable(!slot_info->is_bootable);
    return zx::ok(attributes.flags);
  }

  AbrSlotData ToFuchsia(uint64_t gpt_entry_flags) {
    const KolaGptEntryAttributes attributes(gpt_entry_flags);

    AbrSlotData abr_slot_data = {};
    abr_slot_data.priority = static_cast<uint8_t>(attributes.priority());
    // Both fields are 3 bits wide.
    abr_slot_data.tries_remaining = static_cast<uint8_t>(attributes.retry_count());
    abr_slot_data.successful_boot = static_cast<uint8_t>(attributes.boot_success());
    return abr_slot_data;
  }

  zx::result<> ReadCustom(AbrSlotData* a, AbrSlotData* b, uint8_t* one_shot_recovery) override {
    *a = ToFuchsia(zircon_a_.gpt_partition->flags);
    *b = ToFuchsia(zircon_b_.gpt_partition->flags);

    // TODO(b/348034903): Consider checking that the higher-priority active slot has the active
    // partition type GUIDs.

    *one_shot_recovery = 0;  // not supported
    return zx::ok();
  }

  zx::result<> WriteCustom(const AbrSlotData* a, const AbrSlotData* b,
                           uint8_t one_shot_recovery) override {
    auto detect_slot_switch = [](const AbrSlotData* old_slot, const AbrSlotData* new_slot) {
      return new_slot->priority == kAbrMaxPriority &&
             new_slot->tries_remaining == kAbrMaxTriesRemaining && !new_slot->successful_boot &&
             old_slot->priority < kAbrMaxPriority && old_slot->successful_boot;
    };

    bool slot_switch = detect_slot_switch(a, b);
    bool new_slot_is_b = false;
    if (slot_switch) {
      new_slot_is_b = true;
    } else {
      slot_switch = detect_slot_switch(b, a);
    }

    std::optional<fit::function<void()>> commit_guid_swap;
    if (slot_switch) {
      LOG("Switching active slot from %s\n", new_slot_is_b ? "A to B" : "B to A");
      zx::result result = SwapAbPartitionTypeGuids(new_slot_is_b);
      if (result.is_error()) {
        return result.take_error();
      }
      commit_guid_swap = std::move(result.value());
    }

    auto rollback = fit::defer([&, orig_a_flags = zircon_a_.gpt_partition->flags,
                                orig_b_flags = zircon_b_.gpt_partition->flags] {
      zircon_a_.gpt_partition->flags = orig_a_flags;
      zircon_b_.gpt_partition->flags = orig_b_flags;
    });

    zircon_a_.gpt_partition->flags = ToKola(*a, *b, zircon_a_.gpt_partition->flags);
    zircon_b_.gpt_partition->flags = ToKola(*b, *a, zircon_b_.gpt_partition->flags);

    // SetActiveAndUnbootable() will read the gpt_partition->flags set above.
    zx::result<uint64_t> new_a_flags =
        SetActiveAndUnbootable(kAbrSlotIndexA, zircon_a_.gpt_partition->flags);
    if (new_a_flags.is_error()) {
      return new_a_flags.take_error();
    }
    zx::result<uint64_t> new_b_flags =
        SetActiveAndUnbootable(kAbrSlotIndexB, zircon_b_.gpt_partition->flags);
    if (new_b_flags.is_error()) {
      return new_b_flags.take_error();
    }

    zircon_a_.gpt_partition->flags = new_a_flags.value();
    zircon_b_.gpt_partition->flags = new_b_flags.value();
    rollback.cancel();
    if (commit_guid_swap.has_value()) {
      (*commit_guid_swap)();
    }
    return zx::ok();
  }

  zx::result<fit::function<void()>> SwapAbPartitionTypeGuids(bool new_slot_is_b) {
    // Find all slot-specific partitions.
    std::unordered_map<std::string, gpt_partition_t*> a_partitions;
    std::unordered_map<std::string, gpt_partition_t*> b_partitions;
    for (uint32_t i = 0; i < gpt::kPartitionCount; i++) {
      zx::result<gpt_partition_t*> p = partitioner_->GetGpt()->GetPartition(i);
      if (p.is_error()) {
        continue;
      }
      gpt_partition_t* gpt_entry = p.value();

      char cstring_name[GPT_NAME_LEN / 2 + 1] = {0};
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
      if (memcmp(part_entry->type, type_guid, GPT_GUID_LEN)) {
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

    auto commit_guid_swap = [a_partitions = std::move(a_partitions),
                             b_partitions = std::move(b_partitions)]() mutable {
      // Swap the A/B partition type GUIDs.
      uint8_t type_guid[GPT_GUID_LEN];
      for (const auto& [a_part_name, a_part_entry] : a_partitions) {
        gpt_partition_t* b_part_entry = b_partitions[a_part_name];
        memcpy(type_guid, a_part_entry->type, GPT_GUID_LEN);
        memcpy(a_part_entry->type, b_part_entry->type, GPT_GUID_LEN);
        memcpy(b_part_entry->type, type_guid, GPT_GUID_LEN);
      }
    };
    return zx::ok(std::move(commit_guid_swap));
  }

  zx::result<> Flush() override { return zx::make_result(partitioner_->GetGpt()->Sync()); }

  const KolaPartitioner* partitioner_;
  FindPartitionDetailsResult zircon_a_;
  FindPartitionDetailsResult zircon_b_;
};

zx::result<std::unique_ptr<abr::Client>> KolaPartitioner::CreateAbrClient() const {
  return KolaAbrClient::Create(this);
}

}  // namespace paver
