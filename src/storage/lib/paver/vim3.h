// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_STORAGE_LIB_PAVER_VIM3_H_
#define SRC_STORAGE_LIB_PAVER_VIM3_H_

#include "src/storage/lib/paver/abr-client.h"
#include "src/storage/lib/paver/device-partitioner.h"
#include "src/storage/lib/paver/gpt.h"
#include "src/storage/lib/paver/partition-client.h"

namespace paver {

class Vim3Partitioner : public DevicePartitioner {
 public:
  static zx::result<std::unique_ptr<DevicePartitioner>> Initialize(
      const BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      fidl::ClientEnd<fuchsia_device::Controller> block_device);

  zx::result<std::unique_ptr<abr::Client>> CreateAbrClient() const override;

  const paver::BlockDevices& Devices() const override;

  fidl::UnownedClientEnd<fuchsia_io::Directory> SvcRoot() const override;

  bool IsFvmWithinFtl() const override { return false; }

  bool SupportsPartition(const PartitionSpec& spec) const override;

  zx::result<std::unique_ptr<PartitionClient>> FindPartition(
      const PartitionSpec& spec) const override;

  zx::result<> FinalizePartition(const PartitionSpec& spec) const override { return zx::ok(); }

  zx::result<> WipeFvm() const override;

  zx::result<> ResetPartitionTables() const override;

  zx::result<> ValidatePayload(const PartitionSpec& spec,
                               std::span<const uint8_t> data) const override;

  zx::result<> Flush() const override { return zx::ok(); }
  zx::result<> OnStop() const override { return zx::ok(); }

 private:
  Vim3Partitioner(std::unique_ptr<GptDevicePartitioner> gpt, BlockDevices devfs_devices)
      : gpt_(std::move(gpt)), devfs_devices_(std::move(devfs_devices)) {}

  zx::result<std::unique_ptr<PartitionClient>> GetEmmcBootPartitionClient() const;

  std::unique_ptr<GptDevicePartitioner> gpt_;
  // Some partitions aren't stored in the GPT, so we retain a connector for them.
  BlockDevices devfs_devices_;
};

class Vim3PartitionerFactory : public DevicePartitionerFactory {
 public:
  zx::result<std::unique_ptr<DevicePartitioner>> New(
      const BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      Arch arch, std::shared_ptr<Context> context,
      fidl::ClientEnd<fuchsia_device::Controller> block_device) final;
};

}  // namespace paver

#endif  // SRC_STORAGE_LIB_PAVER_VIM3_H_
