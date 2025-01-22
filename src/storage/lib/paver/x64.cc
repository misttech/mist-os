// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/x64.h"

#include <lib/fidl/cpp/channel.h>
#include <zircon/process.h>
#include <zircon/processargs.h>
#include <zircon/system/public/zircon/errors.h>

#include <algorithm>
#include <iterator>

#include "fidl/fuchsia.system.state/cpp/common_types.h"
#include "gpt/gpt.h"
#include "src/lib/uuid/uuid.h"
#include "src/storage/lib/paver/device-partitioner.h"
#include "src/storage/lib/paver/gpt.h"
#include "src/storage/lib/paver/pave-logging.h"
#include "src/storage/lib/paver/system_shutdown_state.h"
#include "src/storage/lib/paver/utils.h"
#include "src/storage/lib/paver/validation.h"
#include "zircon/hw/gpt.h"

namespace paver {

namespace {

using fuchsia_system_state::SystemPowerState;
using uuid::Uuid;

constexpr size_t kKibibyte = 1024;
constexpr size_t kMebibyte = kKibibyte * 1024;
constexpr size_t kGibibyte = kMebibyte * 1024;

// All X64 boards currently use the legacy partition scheme.
constexpr PartitionScheme kPartitionScheme = PartitionScheme::kLegacy;

// TODO: Remove support after July 9th 2021.
constexpr char kOldEfiName[] = "efi-system";

// Returns the GUID for the given partition, applying EFI-specific mappings.
Uuid PartitionType(Partition partition) {
  if (partition == Partition::kBootloaderA) {
    // Special case for the bootloader which must be an ESP.
    return GUID_EFI_VALUE;
  }
  std::optional<Uuid> type = PartitionTypeGuid(partition, kPartitionScheme);
  // OK to assert; known types for the EfiDevicePartitioner have a GUID
  ZX_ASSERT(type);
  return *type;
}

}  // namespace

zx::result<std::unique_ptr<DevicePartitioner>> EfiDevicePartitioner::Initialize(
    const paver::BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
    Arch arch, fidl::ClientEnd<fuchsia_device::Controller> block_device,
    std::shared_ptr<Context> context) {
  auto status = GptDevicePartitioner::InitializeGpt(devices, svc_root, std::move(block_device));
  if (status.is_error()) {
    return status.take_error();
  }

  if (zx::result find = status->gpt->FindPartition(IsEfiSystemPartition); find.is_error()) {
    return find.take_error();
  }

  auto partitioner =
      WrapUnique(new EfiDevicePartitioner(arch, std::move(status->gpt), std::move(context)));
  if (status->initialize_partition_tables) {
    if (auto status = partitioner->ResetPartitionTables(); status.is_error()) {
      return status.take_error();
    }
  }

  LOG("Successfully initialized EFI Device Partitioner\n");
  return zx::ok(std::move(partitioner));
}

const paver::BlockDevices& EfiDevicePartitioner::Devices() const { return gpt_->devices(); }

fidl::UnownedClientEnd<fuchsia_io::Directory> EfiDevicePartitioner::SvcRoot() const {
  return gpt_->svc_root();
}

bool EfiDevicePartitioner::SupportsPartition(const PartitionSpec& spec) const {
  const PartitionSpec supported_specs[] = {
      PartitionSpec(paver::Partition::kBootloaderA),
      PartitionSpec(paver::Partition::kZirconA),
      PartitionSpec(paver::Partition::kZirconB),
      PartitionSpec(paver::Partition::kZirconR),
      PartitionSpec(paver::Partition::kVbMetaA),
      PartitionSpec(paver::Partition::kVbMetaB),
      PartitionSpec(paver::Partition::kVbMetaR),
      PartitionSpec(paver::Partition::kAbrMeta),
      PartitionSpec(paver::Partition::kFuchsiaVolumeManager),
      PartitionSpec(paver::Partition::kFuchsiaVolumeManager, kOpaqueVolumeContentType),
  };
  return std::any_of(std::cbegin(supported_specs), std::cend(supported_specs),
                     [&](const PartitionSpec& supported) { return SpecMatches(spec, supported); });
}

zx::result<std::unique_ptr<PartitionClient>> EfiDevicePartitioner::FindPartition(
    const PartitionSpec& spec) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  switch (spec.partition) {
    case Partition::kBootloaderA: {
      const auto filter = [](const GptPartitionMetadata& part) {
        return FilterByTypeAndName(part, GUID_EFI_VALUE, GUID_EFI_NAME) ||
               // TODO: Remove support after July 9th 2021.
               FilterByTypeAndName(part, GUID_EFI_VALUE, kOldEfiName);
      };
      auto status = gpt_->FindPartition(filter);
      if (status.is_error()) {
        return status.take_error();
      }
      return zx::ok(std::move(*status));
    }
    case Partition::kZirconA:
    case Partition::kZirconB:
    case Partition::kZirconR:
    case Partition::kVbMetaA:
    case Partition::kVbMetaB:
    case Partition::kVbMetaR:
    case Partition::kAbrMeta: {
      const auto filter = [&spec](const GptPartitionMetadata& part) {
        return FilterByType(part, PartitionType(spec.partition));
      };
      auto status = gpt_->FindPartition(filter);
      if (status.is_error()) {
        return status.take_error();
      }
      return zx::ok(std::move(*status));
    }
    case Partition::kFuchsiaVolumeManager: {
      auto status = gpt_->FindPartition(IsFvmPartition);
      if (status.is_error()) {
        return status.take_error();
      }
      return zx::ok(std::move(*status));
    }
    default:
      ERROR("EFI partitioner cannot find unknown partition type\n");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
}

zx::result<> EfiDevicePartitioner::WipeFvm() const { return gpt_->WipeFvm(); }

zx::result<> EfiDevicePartitioner::ResetPartitionTables() const {
  LOG("Wiping GPT, expect data loss.\n");
  using PartitionInitSpec = GptDevicePartitioner::PartitionInitSpec;

  const std::array<PartitionInitSpec, 9> fuchsia_partitions{
      PartitionInitSpec{
          .name = GUID_EFI_NAME,
          .type = GUID_EFI_VALUE,
          .size_bytes = 16 * kMebibyte,
      },
      PartitionInitSpec::ForKnownPartition(Partition::kZirconA, kPartitionScheme, 128 * kMebibyte),
      PartitionInitSpec::ForKnownPartition(Partition::kZirconB, kPartitionScheme, 128 * kMebibyte),
      PartitionInitSpec::ForKnownPartition(Partition::kZirconR, kPartitionScheme, 192 * kMebibyte),
      PartitionInitSpec::ForKnownPartition(Partition::kVbMetaA, kPartitionScheme, 64 * kKibibyte),
      PartitionInitSpec::ForKnownPartition(Partition::kVbMetaB, kPartitionScheme, 64 * kKibibyte),
      PartitionInitSpec::ForKnownPartition(Partition::kVbMetaR, kPartitionScheme, 64 * kKibibyte),
      PartitionInitSpec::ForKnownPartition(Partition::kAbrMeta, kPartitionScheme, 4 * kKibibyte),
      PartitionInitSpec::ForKnownPartition(Partition::kFuchsiaVolumeManager, kPartitionScheme,
                                           56 * kGibibyte),
  };

  std::vector<PartitionInitSpec> partitions_to_add;
  partitions_to_add.resize(gpt::kPartitionCount);
  size_t index = 0;

  // To support dual-booting, add back any partitions we've found which are not known to Fuchsia.
  zx::result<std::vector<std::unique_ptr<BlockPartitionClient>>> non_fuchsia_partitions =
      gpt_->FindAllPartitions([&](const GptPartitionMetadata& part) -> bool {
        // There are multiple possible partitions with the ESP type GUID.  Filter those which aren't
        // from Fuchsia.
        if (part.type_guid == Uuid(GUID_EFI_VALUE)) {
          return part.name != GUID_EFI_NAME;
        }
        // For everything else, check if it's a known (Fuchsia-specific) type GUID.
        for (const auto& known_partition : fuchsia_partitions) {
          if (part.type_guid == known_partition.type) {
            return false;
          }
        }
        return true;
      });
  if (non_fuchsia_partitions.is_error()) {
    ERROR("Failed to find non-Fuchsia partitions; dual booting may break!: %s\n",
          non_fuchsia_partitions.status_string());
  } else {
    for (auto& partition : non_fuchsia_partitions.value()) {
      zx::result metadata = partition->GetMetadata();
      zx::result size = partition->GetPartitionSize();
      if (metadata.is_ok() && size.is_ok()) {
        LOG("Preserving non-Fuchsia partition %s (%s) @ %" PRIu64 "\n", metadata->name.c_str(),
            metadata->type_guid.ToString().c_str(), metadata->start_block_offset);
        partitions_to_add[index++] = PartitionInitSpec{
            .name = std::move(metadata->name),
            .type = metadata->type_guid,
            .instance = metadata->instance_guid,
            .start_block = metadata->start_block_offset,
            .size_bytes = *size,
            .flags = metadata->flags,
        };
      } else {
        ERROR("Failed to query info for non-Fuchsia partition: %s. Dual booting may break!\n",
              metadata.is_error() ? metadata.status_string() : size.status_string());
      }
      if (index >= partitions_to_add.size()) {
        ERROR("Too many partitions found!\n");
        return zx::error(ZX_ERR_BAD_STATE);
      }
    }
  }

  // Add the known partitions at the end, so the existing partitions get allocated first.
  for (const auto& partition : fuchsia_partitions) {
    partitions_to_add[index++] = partition;
    if (index >= partitions_to_add.size()) {
      ERROR("Too many partitions found!\n");
      return zx::error(ZX_ERR_BAD_STATE);
    }
  }

  return gpt_->ResetPartitionTables(std::move(partitions_to_add));
}

zx::result<> EfiDevicePartitioner::ValidatePayload(const PartitionSpec& spec,
                                                   std::span<const uint8_t> data) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  if (IsZirconPartitionSpec(spec)) {
    if (!IsValidKernelZbi(arch_, data)) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
  }

  return zx::ok();
}

// Helper function to access `abr_client`.
// Used to set one-shot flags to reboot to recovery/bootloader.
zx::result<> EfiDevicePartitioner::CallAbr(
    std::function<zx::result<>(abr::Client&)> call_abr) const {
  auto partition = FindPartition(paver::PartitionSpec(paver::Partition::kAbrMeta));
  if (partition.is_error()) {
    ERROR("Failed to find A/B/R metadata partition: %s\n", partition.status_string());
    return partition.take_error();
  }

  auto abr_partition_client = abr::AbrPartitionClient::Create(std::move(partition.value()));
  if (abr_partition_client.is_error()) {
    ERROR("Failed to create A/B/R metadata partition client: %s\n",
          abr_partition_client.status_string());
    return abr_partition_client.take_error();
  }
  auto& abr_client = abr_partition_client.value();

  return call_abr(*abr_client);
}

zx::result<> EfiDevicePartitioner::OnStop() const {
  const auto state = GetShutdownSystemState(gpt_->svc_root());
  switch (state) {
    case SystemPowerState::kRebootBootloader:
      LOG("Setting one shot reboot to bootloader flag.\n");
      return CallAbr([](abr::Client& abr_client) { return abr_client.SetOneShotBootloader(); });
    case SystemPowerState::kRebootRecovery:
      LOG("Setting one shot reboot to recovery flag\n");
      return CallAbr([](abr::Client& abr_client) { return abr_client.SetOneShotRecovery(); });
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

zx::result<std::unique_ptr<DevicePartitioner>> X64PartitionerFactory::New(
    const paver::BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
    Arch arch, std::shared_ptr<Context> context,
    fidl::ClientEnd<fuchsia_device::Controller> block_device) {
  return EfiDevicePartitioner::Initialize(devices, svc_root, arch, std::move(block_device),
                                          std::move(context));
}

zx::result<std::unique_ptr<abr::Client>> EfiDevicePartitioner::CreateAbrClient() const {
  // ABR metadata has no need of a content type since it's always local rather
  // than provided in an update package, so just use the default content type.
  auto partition = FindPartition(paver::PartitionSpec(paver::Partition::kAbrMeta));
  if (partition.is_error()) {
    ERROR("Failed to find abr partition\n");
    return partition.take_error();
  }

  return abr::AbrPartitionClient::Create(std::move(partition.value()));
}

}  // namespace paver
