// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_STORAGE_LIB_PAVER_PARTITION_CLIENT_H_
#define SRC_STORAGE_LIB_PAVER_PARTITION_CLIENT_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.partition/cpp/wire.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstddef>
#include <optional>
#include <vector>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <fbl/unique_fd.h>
#include <storage/buffer/owned_vmoid.h>

#include "src/devices/block/drivers/core/block-fifo.h"
#include "src/storage/lib/block_client/cpp/client.h"
#include "src/storage/lib/paver/utils.h"

namespace paver {

class BlockPartitionClient;

// Interface to synchronously read/write to a partition.
class PartitionClient {
 public:
  // Returns the block size which the vmo provided to read/write should be aligned to.
  virtual zx::result<size_t> GetBlockSize() = 0;

  // Returns the partition size.
  virtual zx::result<size_t> GetPartitionSize() = 0;

  // Reads the specified size from the partition into |vmo|. |size| must be aligned to the block
  // size returned in `GetBlockSize`.
  virtual zx::result<> Read(const zx::vmo& vmo, size_t size) = 0;

  // Writes |vmo| into the partition. |vmo_size| must be aligned to the block size returned in
  // `GetBlockSize`.
  virtual zx::result<> Write(const zx::vmo& vmo, size_t vmo_size) = 0;

  // Issues a trim to the entire partition.
  virtual zx::result<> Trim() = 0;

  // Flushes all previous operations to persistent storage.
  virtual zx::result<> Flush() = 0;

  // Indicates whether the derrived class supports block operations.
  virtual bool SupportsBlockPartition() { return false; }

  virtual ~PartitionClient() = default;
};

class BlockPartitionClient : public PartitionClient {
 public:
  explicit BlockPartitionClient(fidl::ClientEnd<fuchsia_device::Controller> controller,
                                fidl::ClientEnd<fuchsia_hardware_block::Block> partition)
      : controller_(std::move(controller)), partition_(std::move(partition)) {}

  explicit BlockPartitionClient(PartitionConnection connection)
      : controller_(std::move(connection.controller)),
        partition_(fidl::ClientEnd<fuchsia_hardware_block::Block>(std::move(connection.device))) {}

  static zx::result<std::unique_ptr<BlockPartitionClient>> Create(
      fidl::UnownedClientEnd<fuchsia_device::Controller> partition_controller);

  zx::result<size_t> GetBlockSize() override;
  zx::result<size_t> GetPartitionSize() override;

  zx::result<> Read(const zx::vmo& vmo, size_t size) override;
  zx::result<> Read(const zx::vmo& vmo, size_t size, size_t dev_offset, size_t vmo_offset);

  zx::result<> Write(const zx::vmo& vmo, size_t vmo_size) override;
  zx::result<> Write(const zx::vmo& vmo, size_t vmo_size, size_t dev_offset, size_t vmo_offset);

  zx::result<storage::OwnedVmoid> RegisterVmoid(const zx::vmo& vmo);
  zx::result<> Read(vmoid_t vmoid, size_t vmo_size, size_t dev_offset, size_t vmo_offset);
  zx::result<> Write(vmoid_t vmoid, size_t vmo_size, size_t dev_offset, size_t vmo_offset);

  zx::result<> Trim() override;
  zx::result<> Flush() override;

  fidl::UnownedClientEnd<fuchsia_hardware_block::Block> block_channel();
  fidl::UnownedClientEnd<fuchsia_device::Controller> controller_channel();

  // No copy.
  BlockPartitionClient(const BlockPartitionClient&) = delete;
  BlockPartitionClient& operator=(const BlockPartitionClient&) = delete;
  BlockPartitionClient(BlockPartitionClient&& o) = default;
  BlockPartitionClient& operator=(BlockPartitionClient&&) = default;

  bool SupportsBlockPartition() override { return true; }

 private:
  zx::result<> RegisterFastBlockIo();
  zx::result<std::reference_wrapper<fuchsia_hardware_block::wire::BlockInfo>> ReadBlockInfo();

  fidl::WireSyncClient<fuchsia_device::Controller> controller_;
  fidl::WireSyncClient<fuchsia_hardware_block::Block> partition_;
  std::unique_ptr<block_client::Client> client_;
  std::optional<fuchsia_hardware_block::wire::BlockInfo> block_info_;
};

// A variant of BlockPartitionClient that reads/writes starting from a fixed offset in
// the partition and from a fixed offset in the given buffer.
// This is for those cases where image doesn't necessarily start from the beginning of
// the partition, (i.e. for preserving metatdata/header).
// It's also used for cases where input image is a combined image for multiple partitions.
class FixedOffsetBlockPartitionClient final : public BlockPartitionClient {
 public:
  explicit FixedOffsetBlockPartitionClient(fidl::ClientEnd<fuchsia_device::Controller> controller,
                                           fidl::ClientEnd<fuchsia_hardware_block::Block> partition,
                                           size_t offset_partition_in_blocks,
                                           size_t offset_buffer_in_blocks)
      : BlockPartitionClient(std::move(controller), std::move(partition)),
        offset_partition_in_blocks_(offset_partition_in_blocks),
        offset_buffer_in_blocks_(offset_buffer_in_blocks) {}

  explicit FixedOffsetBlockPartitionClient(PartitionConnection connection,
                                           size_t offset_partition_in_blocks,
                                           size_t offset_buffer_in_blocks)
      : BlockPartitionClient(std::move(connection)),
        offset_partition_in_blocks_(offset_partition_in_blocks),
        offset_buffer_in_blocks_(offset_buffer_in_blocks) {}

  explicit FixedOffsetBlockPartitionClient(BlockPartitionClient client,
                                           size_t offset_partition_in_blocks,
                                           size_t offset_buffer_in_blocks)
      : BlockPartitionClient(std::move(client)),
        offset_partition_in_blocks_(offset_partition_in_blocks),
        offset_buffer_in_blocks_(offset_buffer_in_blocks) {}

  static zx::result<std::unique_ptr<FixedOffsetBlockPartitionClient>> Create(
      fidl::UnownedClientEnd<fuchsia_device::Controller> partition_controller,
      size_t offset_partition_in_blocks, size_t offset_buffer_in_blocks);

  zx::result<size_t> GetPartitionSize() final;
  zx::result<> Read(const zx::vmo& vmo, size_t size) final;
  zx::result<> Write(const zx::vmo& vmo, size_t vmo_size) final;

  zx::result<> Read(vmoid_t vmoid, size_t vmo_size, size_t dev_offset, size_t vmo_offset);
  zx::result<> Write(vmoid_t vmoid, size_t vmo_size, size_t dev_offset, size_t vmo_offset);

  // No copy, no move.
  FixedOffsetBlockPartitionClient(const FixedOffsetBlockPartitionClient&) = delete;
  FixedOffsetBlockPartitionClient& operator=(const FixedOffsetBlockPartitionClient&) = delete;
  FixedOffsetBlockPartitionClient(FixedOffsetBlockPartitionClient&&) = delete;
  FixedOffsetBlockPartitionClient& operator=(FixedOffsetBlockPartitionClient&&) = delete;

  zx::result<size_t> GetBufferOffsetInBytes();

 private:
  // offset in blocks for partition
  size_t offset_partition_in_blocks_ = 0;
  // offset in blocks for the input buffer
  size_t offset_buffer_in_blocks_ = 0;
};

// Specialized partition client which duplicates to multiple partitions, and attempts to read from
// each.
class PartitionCopyClient final : public PartitionClient {
 public:
  explicit PartitionCopyClient(std::vector<std::unique_ptr<PartitionClient>> partitions)
      : partitions_(std::move(partitions)) {}

  zx::result<size_t> GetBlockSize() final;
  zx::result<size_t> GetPartitionSize() final;
  zx::result<> Read(const zx::vmo& vmo, size_t size) final;
  zx::result<> Write(const zx::vmo& vmo, size_t vmo_size) final;
  zx::result<> Trim() final;
  zx::result<> Flush() final;

  // No copy, no move.
  PartitionCopyClient(const PartitionCopyClient&) = delete;
  PartitionCopyClient& operator=(const PartitionCopyClient&) = delete;
  PartitionCopyClient(PartitionCopyClient&&) = delete;
  PartitionCopyClient& operator=(PartitionCopyClient&&) = delete;

 private:
  std::vector<std::unique_ptr<PartitionClient>> partitions_;
};

}  // namespace paver

#endif  // SRC_STORAGE_LIB_PAVER_PARTITION_CLIENT_H_
