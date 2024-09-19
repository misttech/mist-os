// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_PAVER_KOLA_H_
#define SRC_STORAGE_LIB_PAVER_KOLA_H_

#include "src/storage/lib/paver/abr-client.h"
#include "src/storage/lib/paver/device-partitioner.h"
#include "src/storage/lib/paver/gpt.h"
#include "src/storage/lib/paver/partition-client.h"

namespace paver {

using FindPartitionResult = GptDevicePartitioner::FindPartitionResult;

class KolaPartitioner : public DevicePartitioner {
 public:
  static zx::result<std::unique_ptr<DevicePartitioner>> Initialize(
      const BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      fidl::ClientEnd<fuchsia_device::Controller> block_device);

  bool IsFvmWithinFtl() const override { return false; }

  bool SupportsPartition(const PartitionSpec& spec) const override;

  zx::result<std::unique_ptr<PartitionClient>> AddPartition(
      const PartitionSpec& spec) const override;

  zx::result<std::unique_ptr<PartitionClient>> FindPartition(
      const PartitionSpec& spec) const override;

  zx::result<> FinalizePartition(const PartitionSpec& spec) const override;

  zx::result<> WipeFvm() const override;

  zx::result<> InitPartitionTables() const override;

  zx::result<> WipePartitionTables() const override;

  zx::result<> ValidatePayload(const PartitionSpec& spec,
                               cpp20::span<const uint8_t> data) const override;

  zx::result<> Flush() const override { return zx::ok(); }

  zx::result<> OnStop() const override;

  // Like FindPartition() above, but also returns the GPT partition entry.
  zx::result<FindPartitionResult> FindPartitionDetails(const PartitionSpec& spec) const;

  GptDevice* GetGpt() const { return gpt_->GetGpt(); }

 private:
  KolaPartitioner(std::unique_ptr<GptDevicePartitioner> gpt) : gpt_(std::move(gpt)) {}

  std::unique_ptr<GptDevicePartitioner> gpt_;
};

class KolaPartitionerFactory : public DevicePartitionerFactory {
 public:
  zx::result<std::unique_ptr<DevicePartitioner>> New(
      const BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      Arch arch, std::shared_ptr<Context> context,
      fidl::ClientEnd<fuchsia_device::Controller> block_device) final;
};

class KolaAbrClientFactory : public abr::ClientFactory {
 public:
  zx::result<std::unique_ptr<abr::Client>> New(
      const BlockDevices& devices, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      std::shared_ptr<paver::Context> context) final;
};

}  // namespace paver

#endif  // SRC_STORAGE_LIB_PAVER_KOLA_H_
