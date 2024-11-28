// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_STORAGE_LIB_PAVER_NELSON_H_
#define SRC_STORAGE_LIB_PAVER_NELSON_H_

#include "src/storage/lib/paver/abr-client.h"
#include "src/storage/lib/paver/block-devices.h"
#include "src/storage/lib/paver/device-partitioner.h"
#include "src/storage/lib/paver/gpt.h"
#include "src/storage/lib/paver/partition-client.h"

namespace paver {

constexpr size_t kNelsonBL2Size = 64 * 1024;

class NelsonPartitioner : public DevicePartitioner {
 public:
  static zx::result<std::unique_ptr<DevicePartitioner>> Initialize(
      const paver::BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      fidl::ClientEnd<fuchsia_device::Controller> block_device);

  zx::result<std::unique_ptr<abr::Client>> CreateAbrClient() const override;

  const paver::BlockDevices& Devices() const override;

  fidl::UnownedClientEnd<fuchsia_io::Directory> SvcRoot() const override;

  bool IsFvmWithinFtl() const override { return false; }

  bool SupportsPartition(const PartitionSpec& spec) const override;

  zx::result<std::unique_ptr<PartitionClient>> FindPartition(
      const PartitionSpec& spec) const override;

  zx::result<> WipeFvm() const override;

  zx::result<> ResetPartitionTables() const override;

  zx::result<> ValidatePayload(const PartitionSpec& spec,
                               std::span<const uint8_t> data) const override;

  zx::result<> Flush() const override { return zx::ok(); }
  zx::result<> OnStop() const override { return zx::ok(); }

 private:
  explicit NelsonPartitioner(std::unique_ptr<GptDevicePartitioner> gpt) : gpt_(std::move(gpt)) {}

  zx::result<std::unique_ptr<PartitionClient>> GetEmmcBootPartitionClient() const;

  zx::result<std::unique_ptr<PartitionClient>> GetBootloaderPartitionClient(
      const PartitionSpec& spec) const;

  std::unique_ptr<GptDevicePartitioner> gpt_;
};

class NelsonPartitionerFactory : public DevicePartitionerFactory {
 public:
  zx::result<std::unique_ptr<DevicePartitioner>> New(
      const paver::BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      Arch arch, std::shared_ptr<Context> context,
      fidl::ClientEnd<fuchsia_device::Controller> block_device) final;
};

class NelsonBootloaderPartitionClient final : public PartitionClient {
 public:
  explicit NelsonBootloaderPartitionClient(
      std::unique_ptr<PartitionClient> emmc_boot_client,
      std::unique_ptr<FixedOffsetBlockPartitionClient> tpl_client)
      : emmc_boot_client_(std::move(emmc_boot_client)), tpl_client_(std::move(tpl_client)) {}

  zx::result<size_t> GetBlockSize() final;
  zx::result<size_t> GetPartitionSize() final;
  zx::result<> Read(const zx::vmo& vmo, size_t size) final;
  zx::result<> Write(const zx::vmo& vmo, size_t vmo_size) final;
  zx::result<> Trim() final;
  zx::result<> Flush() final;

  // No copy, no move.
  NelsonBootloaderPartitionClient(const NelsonBootloaderPartitionClient&) = delete;
  NelsonBootloaderPartitionClient& operator=(const NelsonBootloaderPartitionClient&) = delete;
  NelsonBootloaderPartitionClient(NelsonBootloaderPartitionClient&&) = delete;
  NelsonBootloaderPartitionClient& operator=(NelsonBootloaderPartitionClient&&) = delete;

 private:
  bool CheckIfTplSame(const zx::vmo& vmo, size_t size);

  std::unique_ptr<PartitionClient> emmc_boot_client_;
  std::unique_ptr<FixedOffsetBlockPartitionClient> tpl_client_;
};
}  // namespace paver

#endif  // SRC_STORAGE_LIB_PAVER_NELSON_H_
