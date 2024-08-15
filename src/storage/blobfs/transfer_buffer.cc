// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/transfer_buffer.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>
#include <memory>
#include <utility>
#include <vector>

#include <fbl/algorithm.h>
#include <storage/buffer/owned_vmoid.h>
#include <storage/operation/operation.h>

#include "src/storage/blobfs/blobfs_metrics.h"
#include "src/storage/blobfs/compression_settings.h"
#include "src/storage/blobfs/format.h"
#include "src/storage/blobfs/iterator/block_iterator.h"
#include "src/storage/blobfs/iterator/block_iterator_provider.h"
#include "src/storage/blobfs/loader_info.h"
#include "src/storage/blobfs/transaction_manager.h"
#include "src/storage/lib/trace/trace.h"
#include "src/storage/lib/vfs/cpp/ticker.h"

namespace blobfs {

StorageBackedTransferBuffer::StorageBackedTransferBuffer(zx::vmo vmo, size_t size,
                                                         storage::OwnedVmoid vmoid,
                                                         TransactionManager* txn_manager,
                                                         BlockIteratorProvider* block_iter_provider,
                                                         BlobfsMetrics* metrics)
    : txn_manager_(txn_manager),
      block_iter_provider_(block_iter_provider),
      vmo_(std::move(vmo)),
      size_(size),
      vmoid_(std::move(vmoid)),
      metrics_(metrics) {}

zx::result<std::unique_ptr<StorageBackedTransferBuffer>> StorageBackedTransferBuffer::Create(
    size_t size, TransactionManager* txn_manager, BlockIteratorProvider* block_iter_provider,
    BlobfsMetrics* metrics) {
  ZX_DEBUG_ASSERT(metrics != nullptr && txn_manager != nullptr && block_iter_provider != nullptr);
  if (size % kBlobfsBlockSize != 0 || size % zx_system_get_page_size() != 0) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  zx::vmo vmo;
  zx_status_t status = zx::vmo::create(size, 0, &vmo);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Cannot create pager transfer buffer";
    return zx::error(status);
  }
  storage::OwnedVmoid vmoid(txn_manager);
  status = vmoid.AttachVmo(vmo);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to attach pager transfer vmo";
    return zx::error(status);
  }

  return zx::ok(std::unique_ptr<StorageBackedTransferBuffer>(new StorageBackedTransferBuffer(
      std::move(vmo), size, std::move(vmoid), txn_manager, block_iter_provider, metrics)));
}

zx::result<> StorageBackedTransferBuffer::Populate(uint64_t offset, uint64_t length,
                                                   const LoaderInfo& info) {
  // Currently our block size is saved as a variable in some places and uses a constant in others.
  // These should always match.
  ZX_ASSERT(info.layout->blobfs_block_size() == kBlobfsBlockSize);

  fs::Ticker ticker;
  if (offset % kBlobfsBlockSize != 0) {
    // The block math below relies on the offset being block-aligned.
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto block_iter = block_iter_provider_->BlockIteratorByNodeIndex(info.node_index);
  if (block_iter.is_error()) {
    return block_iter.take_error();
  }

  uint64_t start_block = (info.layout->DataOffset() + offset) / kBlobfsBlockSize;
  uint64_t block_count = fbl::round_up(length, kBlobfsBlockSize) / kBlobfsBlockSize;

  TRACE_DURATION("blobfs", "StorageBackedTransferBuffer::Populate", "offset",
                 start_block * kBlobfsBlockSize, "length", block_count * kBlobfsBlockSize);

  // Pre-commit pages to the VMO. Committing all of the pages at once can make copying data into the
  // VMO faster
  if (zx_status_t status =
          vmo_.op_range(ZX_VMO_OP_COMMIT, 0, block_count * kBlobfsBlockSize, nullptr, 0);
      status != ZX_OK) {
    FX_PLOGS(WARNING, status) << "Failed to commit vmo";
    // This is only an optimization, it's fine it it fails.
  }

  // Navigate to the start block.
  zx_status_t status = IterateToBlock(&block_iter.value(), start_block);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to navigate to start block " << start_block;
    return zx::error(status);
  }

  std::vector<storage::BufferedOperation> operations;
  // Enqueue operations to read in the required blocks to the transfer buffer.
  const uint64_t data_start = DataStartBlock(txn_manager_->Info());
  status = StreamBlocks(&block_iter.value(), block_count,
                        [&](uint64_t vmo_offset, uint64_t dev_offset, uint64_t length) {
                          operations.push_back({.vmoid = vmoid_.get(),
                                                .op = {
                                                    .type = storage::OperationType::kRead,
                                                    .vmo_offset = vmo_offset - start_block,
                                                    .dev_offset = dev_offset + data_start,
                                                    .length = length,
                                                }});
                          return ZX_OK;
                        });
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to enqueue read operations";
    return zx::error(status);
  }

  // Issue the read.
  status = txn_manager_->RunRequests(operations);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to transact read operations";
    return zx::error(status);
  }

  // Update read metrics
  if (info.decompressor == nullptr) {
    metrics_->paged_read_metrics().IncrementDiskRead(CompressionAlgorithm::kUncompressed,
                                                     block_count * kBlobfsBlockSize, ticker.End());
  } else {
    metrics_->paged_read_metrics().IncrementDiskRead(info.decompressor->algorithm(),
                                                     block_count * kBlobfsBlockSize, ticker.End());
  }
  return zx::ok();
}

}  // namespace blobfs
