// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/violet.h"

#include <lib/stdcompat/span.h>

#include <algorithm>
#include <iterator>

#include <gpt/gpt.h>

#include "src/lib/uuid/uuid.h"
#include "src/storage/lib/paver/pave-logging.h"
#include "src/storage/lib/paver/utils.h"

namespace paver {
namespace {

using uuid::Uuid;

}  // namespace

zx::result<std::unique_ptr<DevicePartitioner>> VioletPartitioner::Initialize(
    fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
    fidl::ClientEnd<fuchsia_device::Controller> block_device) {
  auto status = IsBoard(devfs_root, "violet");
  if (status.is_error()) {
    return status.take_error();
  }

  auto status_or_gpt =
      GptDevicePartitioner::InitializeGpt(std::move(devfs_root), svc_root, std::move(block_device));
  if (status_or_gpt.is_error()) {
    return status_or_gpt.take_error();
  }

  auto partitioner = WrapUnique(new VioletPartitioner(std::move(status_or_gpt->gpt)));

  LOG("Successfully initialized VioletPartitioner Device Partitioner\n");
  return zx::ok(std::move(partitioner));
}

bool VioletPartitioner::SupportsPartition(const PartitionSpec& spec) const {
  const PartitionSpec supported_specs[] = {PartitionSpec(paver::Partition::kZirconA)};
  return std::any_of(std::cbegin(supported_specs), std::cend(supported_specs),
                     [&](const PartitionSpec& supported) { return SpecMatches(spec, supported); });
}

zx::result<std::unique_ptr<PartitionClient>> VioletPartitioner::AddPartition(
    const PartitionSpec& spec) const {
  ERROR("Cannot add partitions to a violet device\n");
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<std::unique_ptr<PartitionClient>> VioletPartitioner::FindPartition(
    const PartitionSpec& spec) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  std::variant<std::string_view, Uuid> part_info;
  switch (spec.partition) {
    case Partition::kZirconA:
      part_info = "boot";
      break;
    default:
      ERROR("Partition type is invalid\n");
      return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const auto filter_by_name = [&part_info](const gpt_partition_t& part) {
    char cstring_name[GPT_NAME_LEN] = {};
    utf16_to_cstring(cstring_name, part.name, GPT_NAME_LEN);
    return std::get<std::string_view>(part_info) == std::string_view(cstring_name);
  };

  if (std::holds_alternative<Uuid>(part_info)) {
    auto partition =
        OpenBlockPartition(gpt_->devfs_root(), std::nullopt, std::get<Uuid>(part_info), ZX_SEC(5));
    if (partition.is_error()) {
      return partition.take_error();
    }
    return zx::ok(new BlockPartitionClient(std::move(partition.value())));
  }
  auto status = gpt_->FindPartition(std::move(filter_by_name));
  if (status.is_error()) {
    return status.take_error();
  }
  return zx::ok(std::move(status->partition));
}

zx::result<> VioletPartitioner::WipeFvm() const { return gpt_->WipeFvm(); }

zx::result<> VioletPartitioner::InitPartitionTables() const {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> VioletPartitioner::WipePartitionTables() const {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> VioletPartitioner::ValidatePayload(const PartitionSpec& spec,
                                                cpp20::span<const uint8_t> data) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  return zx::ok();
}

zx::result<std::unique_ptr<DevicePartitioner>> VioletPartitionerFactory::New(
    fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root, Arch arch,
    std::shared_ptr<Context> context, fidl::ClientEnd<fuchsia_device::Controller> block_device) {
  return VioletPartitioner::Initialize(std::move(devfs_root), svc_root, std::move(block_device));
}

zx::result<std::unique_ptr<abr::Client>> VioletAbrClientFactory::New(
    fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
    std::shared_ptr<paver::Context> context) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

}  // namespace paver
