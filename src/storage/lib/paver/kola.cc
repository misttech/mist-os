// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/kola.h"

#include <lib/zx/result.h>

#include <algorithm>
#include <iterator>
#include <string>

#include <gpt/gpt.h>

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
  auto status = IsBoard(devices.devfs_root(), "kola");
  if (status.is_error()) {
    return status.take_error();
  }

  auto gpt = GptDevicePartitioner::InitializeGpt(devices, svc_root, std::move(block_device));
  if (gpt.is_error()) {
    return gpt.take_error();
  }

  auto partitioner = WrapUnique(new KolaPartitioner(std::move(gpt->gpt)));

  LOG("Successfully initialized Kola Device Partitioner\n");
  return zx::ok(std::move(partitioner));
}

bool KolaPartitioner::SupportsPartition(const PartitionSpec& spec) const {
  constexpr PartitionSpec supported_specs[] = {
      PartitionSpec(paver::Partition::kZirconA), PartitionSpec(paver::Partition::kZirconB),
      PartitionSpec(paver::Partition::kFuchsiaVolumeManager)};
  return std::any_of(std::cbegin(supported_specs), std::cend(supported_specs),
                     [&](const PartitionSpec& supported) { return SpecMatches(spec, supported); });
}

zx::result<std::unique_ptr<PartitionClient>> KolaPartitioner::AddPartition(
    const PartitionSpec& spec) const {
  ERROR("Cannot add partitions to a Kola device\n");
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<std::unique_ptr<PartitionClient>> KolaPartitioner::FindPartition(
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

  const auto filter = [&](const gpt_partition_t& part) { return FilterByName(part, part_name); };
  auto status = gpt_->FindPartition(filter);
  if (status.is_error()) {
    return status.take_error();
  }

  if (spec.partition == Partition::kFuchsiaVolumeManager) {
    return zx::ok(std::move(status->partition));
  }

  // TODO(b/348034903): Support ABR updates.
  // Affect both A and B slots, because we do not have support for ABR yet.
  part_name = spec.partition == Partition::kZirconA ? "boot_b" : "boot_a";
  auto other = gpt_->FindPartition(filter);
  if (other.is_error()) {
    return other.take_error();
  }

  std::vector<std::unique_ptr<PartitionClient>> partitions;
  partitions.push_back(std::move(status->partition));
  partitions.push_back(std::move(other->partition));

  return zx::ok(std::make_unique<PartitionCopyClient>(std::move(partitions)));
}

zx::result<> KolaPartitioner::FinalizePartition(const PartitionSpec& spec) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  return zx::make_result(gpt_->GetGpt()->Sync());
}

zx::result<> KolaPartitioner::WipeFvm() const { return gpt_->WipeFvm(); }

zx::result<> KolaPartitioner::InitPartitionTables() const {
  ERROR("Initialising partition tables is not supported for a Kola device\n");
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> KolaPartitioner::WipePartitionTables() const {
  ERROR("Wiping partition tables is not supported for a Kola device\n");
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> KolaPartitioner::ValidatePayload(const PartitionSpec& spec,
                                              cpp20::span<const uint8_t> data) const {
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

// TODO(b/348034903): Support ABR updates.
// This is currently a no-op abr::Client for Kola, because we do not have support for ABR yet.
class KolaAbrClient : public abr::Client {
 public:
  static zx::result<std::unique_ptr<abr::Client>> Create(const paver::BlockDevices& devices) {
    auto status = IsBoard(devices.devfs_root(), "kola");
    if (status.is_error()) {
      return status.take_error();
    }
    return zx::ok(new KolaAbrClient());
  }

 private:
  KolaAbrClient() : Client(/*custom=*/true) { LOG("Successfully initialized Kola Abr Client\n"); }

  zx::result<> Read(uint8_t* buffer, size_t size) override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zx::result<> Write(const uint8_t* buffer, size_t size) override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zx::result<> ReadCustom(AbrSlotData* a, AbrSlotData* b, uint8_t* one_shot_recovery) override {
    constexpr AbrSlotData a_slot_data = {
        .priority = kAbrMaxPriority,
        .tries_remaining = kAbrMaxTriesRemaining,
        .successful_boot = 1,
    };
    constexpr AbrSlotData b_slot_data = {
        .priority = 1,
        .tries_remaining = kAbrMaxTriesRemaining,
        .successful_boot = 1,
    };

    *a = a_slot_data;
    *b = b_slot_data;
    *one_shot_recovery = 0;  // not supported
    return zx::ok();
  }

  zx::result<> WriteCustom(const AbrSlotData* a, const AbrSlotData* b,
                           uint8_t one_shot_recovery) override {
    return zx::ok();
  }

  zx::result<> Flush() const override { return zx::ok(); }
};

zx::result<std::unique_ptr<abr::Client>> KolaAbrClientFactory::New(
    const BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
    std::shared_ptr<paver::Context> context) {
  return KolaAbrClient::Create(devices);
}

}  // namespace paver
