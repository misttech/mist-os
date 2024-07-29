// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/page_loader.h"

#include <lib/fit/defer.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/scheduler/role.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/watchdog/operations.h>
#include <lib/watchdog/watchdog.h>
#include <lib/zx/result.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <algorithm>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <memory>
#include <mutex>
#include <utility>
#include <vector>

#include <fbl/algorithm.h>
#include <fbl/string.h>

#include "src/storage/blobfs/blobfs_metrics.h"
#include "src/storage/blobfs/compression/external_decompressor.h"
#include "src/storage/blobfs/compression/seekable_decompressor.h"
#include "src/storage/blobfs/compression_settings.h"
#include "src/storage/blobfs/format.h"
#include "src/storage/blobfs/loader_info.h"
#include "src/storage/blobfs/transfer_buffer.h"
#include "src/storage/lib/trace/trace.h"
#include "src/storage/lib/vfs/cpp/ticker.h"

namespace blobfs {
namespace {

struct ReadRange {
  uint64_t offset;
  uint64_t length;
};

// Returns a range which covers [offset, offset+length), adjusted for alignment.
//
// The returned range will have the following guarantees:
//  - The range will contain [offset, offset+length).
//  - The returned offset will be block-aligned.
//  - The end of the returned range is *either* block-aligned or is the end of the file.
//  - The range will be adjusted for verification (see |BlobVerifier::Align|).
//
// The range needs to be extended before actually populating the transfer buffer with pages, as
// absent pages will cause page faults during verification on the userpager thread, causing it to
// block against itself indefinitely.
//
// For example:
//                  |...input_range...|
// |..data_block..|..data_block..|..data_block..|
//                |........output_range.........|
ReadRange GetBlockAlignedReadRange(const LoaderInfo& info, uint64_t offset, uint64_t length) {
  uint64_t uncompressed_byte_length = info.layout->FileSize();
  ZX_DEBUG_ASSERT(offset < uncompressed_byte_length);
  // Clamp the range to the size of the blob.
  length = std::min(length, uncompressed_byte_length - offset);

  // Align to the block size for verification. (In practice this means alignment to 8k).
  zx_status_t status = info.verifier->Align(&offset, &length);
  // This only happens if the info.verifier thinks that [offset,length) is out of range, which
  // will only happen if |verifier| was initialized with a different length than the rest of |info|
  // (which is a programming error).
  ZX_DEBUG_ASSERT(status == ZX_OK);

  ZX_DEBUG_ASSERT(offset % kBlobfsBlockSize == 0);
  ZX_DEBUG_ASSERT(length % kBlobfsBlockSize == 0 || offset + length == uncompressed_byte_length);

  return {.offset = offset, .length = length};
}

// Returns a range at least as big as GetBlockAlignedReadRange(), extended by an implementation
// defined read-ahead algorithm.
//
// The same alignment guarantees for GetBlockAlignedReadRange() apply.
ReadRange GetBlockAlignedExtendedRange(const LoaderInfo& info, uint64_t offset, uint64_t length) {
  // TODO(rashaeqbal): Consider making the cluster size dynamic once we have prefetch read
  // efficiency metrics from the kernel - i.e. what percentage of prefetched pages are actually
  // used. Note that dynamic prefetch sizing might not play well with compression, since we
  // always need to read in entire compressed frames.
  //
  // TODO(rashaeqbal): Consider extending the range backwards as well. Will need some way to track
  // populated ranges.
  //
  // Read in at least 32KB at a time. This gives us the best performance numbers w.r.t. memory
  // savings and observed latencies. Detailed results from experiments to tune this can be found in
  // https://fxbug.dev/42125385.
  constexpr uint64_t kReadAheadClusterSize{UINT64_C(32) * (1 << 10)};

  size_t read_ahead_offset = offset;
  size_t read_ahead_length = std::max(kReadAheadClusterSize, length);
  read_ahead_length = std::min(read_ahead_length, info.layout->FileSize() - read_ahead_offset);

  // Align to the block size for verification. (In practice this means alignment to 8k).
  return GetBlockAlignedReadRange(info, read_ahead_offset, read_ahead_length);
}

// A functor that decommits pages from a vmo when called.
class VmoDecommitter {
 public:
  VmoDecommitter(const zx::vmo& vmo, uint64_t offset, uint64_t length)
      : vmo_(vmo), offset_(offset), length_(length) {}

  void operator()() { vmo_.op_range(ZX_VMO_OP_DECOMMIT, offset_, length_, nullptr, 0); }

 private:
  const zx::vmo& vmo_;
  const uint64_t offset_;
  const uint64_t length_;
};

void PopulatePageTable(const fzl::VmoMapper& mapper, uint64_t length) {
  if (zx_status_t status = zx::vmar::root_self()->op_range(
          ZX_VMAR_OP_MAP_RANGE, reinterpret_cast<uint64_t>(mapper.start()), length, nullptr, 0);
      status != ZX_OK) {
    FX_PLOGS(WARNING, status) << "Failed to populate page table";
    // This is only an optimization, it's fine if it fails.
  }
}

}  // namespace

void SetDeadlineProfile(const std::vector<zx::unowned_thread>& threads) {
  // Apply role to each thread.
  const char role[]{"fuchsia.storage.blobfs.pager"};
  for (const auto& thread : threads) {
    const zx_status_t status = fuchsia_scheduler::SetRoleForThread(thread->borrow(), role);
    if (status != ZX_OK) {
      FX_PLOGS(WARNING, status) << "Failed to set role " << role;
      continue;
    }
  }
}

PageLoader::Worker::Worker(size_t decompression_buffer_size, BlobfsMetrics* metrics)
    : decompression_buffer_size_(decompression_buffer_size), metrics_(metrics) {}

zx::result<std::unique_ptr<PageLoader::Worker>> PageLoader::Worker::Create(
    std::unique_ptr<WorkerResources> resources, size_t decompression_buffer_size,
    BlobfsMetrics* metrics, DecompressorCreatorConnector* decompression_connector) {
  ZX_DEBUG_ASSERT(metrics != nullptr && resources->uncompressed_buffer != nullptr &&
                  resources->uncompressed_buffer->GetVmo().is_valid() &&
                  resources->compressed_buffer != nullptr &&
                  resources->compressed_buffer->GetVmo().is_valid());

  if (resources->uncompressed_buffer->GetSize() % kBlobfsBlockSize ||
      resources->compressed_buffer->GetSize() % kBlobfsBlockSize ||
      decompression_buffer_size % kBlobfsBlockSize) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (resources->compressed_buffer->GetSize() < decompression_buffer_size) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  TRACE_DURATION("blobfs", "PageLoader::Worker::Create");

  auto worker = std::unique_ptr<PageLoader::Worker>(
      new PageLoader::Worker(decompression_buffer_size, metrics));
  worker->uncompressed_transfer_buffer_ = std::move(resources->uncompressed_buffer);
  worker->compressed_transfer_buffer_ = std::move(resources->compressed_buffer);

  if (zx_status_t status = worker->uncompressed_transfer_buffer_mapper_.Map(
          worker->uncompressed_transfer_buffer_->GetVmo(), 0,
          worker->uncompressed_transfer_buffer_->GetSize(), ZX_VM_PERM_READ | ZX_VM_PERM_WRITE);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to map uncompressed transfer buffer";
    return zx::error(status);
  }

  if (zx_status_t status =
          zx::vmo::create(worker->decompression_buffer_size_, 0, &worker->decompression_buffer_);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to create decompression buffer";
    return zx::error(status);
  }
  if (zx_status_t status = worker->decompression_buffer_mapper_.Map(
          worker->decompression_buffer_, 0, worker->decompression_buffer_size_, ZX_VM_PERM_READ);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to map decompression buffer";
    return zx::error(status);
  }

  ZX_ASSERT(decompression_connector);
  if (zx_status_t status = zx::vmo::create(kDecompressionBufferSize, 0, &worker->sandbox_buffer_);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to create sandbox buffer";
    return zx::error(status);
  }

  auto client_or =
      ExternalDecompressorClient::Create(decompression_connector, worker->sandbox_buffer_,
                                         worker->compressed_transfer_buffer_->GetVmo());
  if (!client_or.is_ok()) {
    return zx::error(client_or.status_value());
  }
  worker->decompressor_client_ = std::move(client_or.value());

  return zx::ok(std::move(worker));
}

PagerErrorStatus PageLoader::Worker::TransferPages(const PageLoader::PageSupplier& page_supplier,
                                                   uint64_t offset, uint64_t length,
                                                   const LoaderInfo& info) {
  size_t end;
  if (add_overflow(offset, length, &end)) {
    FX_LOGS(ERROR) << "pager transfer range would overflow (off=" << offset << ", len=" << length
                   << ") for blob " << info.verifier->digest();
    return PagerErrorStatus::kErrBadState;
  }

  if (info.decompressor)
    return TransferChunkedPages(page_supplier, offset, length, info);
  return TransferUncompressedPages(page_supplier, offset, length, info);
}

// The requested range is aligned in multiple steps as follows:
// 1. The range is extended to speculatively read in 32k at a time.
// 2. The extended range is further aligned for Merkle tree verification later.
// 3. This range is read in chunks equal to the size of the uncompressed_transfer_buffer_. Each
// chunk is verified as it is read in, and spliced into the destination VMO with supply_pages().
//
// The assumption here is that the transfer buffer is sized per the alignment requirements for
// Merkle tree verification. We have static asserts in place to check this assumption - the transfer
// buffer (256MB) is 8k block aligned.
PagerErrorStatus PageLoader::Worker::TransferUncompressedPages(
    const PageLoader::PageSupplier& page_supplier, uint64_t requested_offset,
    uint64_t requested_length, const LoaderInfo& info) {
  ZX_DEBUG_ASSERT(!info.decompressor);

  const auto [start_offset, total_length] =
      GetBlockAlignedExtendedRange(info, requested_offset, requested_length);

  TRACE_DURATION("blobfs", "PageLoader::TransferUncompressedPages", "offset", start_offset,
                 "length", total_length);

  uint64_t offset = start_offset;
  uint64_t length_remaining = total_length;
  uint64_t length;

  // Read in multiples of the transfer buffer size. In practice we should only require one iteration
  // for the majority of cases, since the transfer buffer is 256MB.
  while (length_remaining > 0) {
    length = std::min(uncompressed_transfer_buffer_->GetSize(), length_remaining);
    uint64_t block_aligned_length = fbl::round_up(length, kBlobfsBlockSize);
    uint64_t page_aligned_length = fbl::round_up(length, zx_system_get_page_size());

    // Decommit pages in the transfer buffer that might have been populated. All blobs share the
    // same transfer buffer - this prevents data leaks between different blobs.
    auto decommit = fit::defer(
        VmoDecommitter(uncompressed_transfer_buffer_->GetVmo(), 0, block_aligned_length));

    // Read from storage into the transfer buffer.
    auto populate_status = uncompressed_transfer_buffer_->Populate(offset, length, info);
    if (!populate_status.is_ok()) {
      FX_LOGS(ERROR) << "TransferUncompressed: Failed to populate transfer vmo for blob "
                     << info.verifier->digest() << ": " << populate_status.status_string()
                     << ". Returning as plain IO error.";
      return PagerErrorStatus::kErrIO;
    }

    // The VMO is already mapped but the pages may not be in the page table. Add all of the pages to
    // the page table at once.
    PopulatePageTable(uncompressed_transfer_buffer_mapper_, page_aligned_length);

    // If |length| isn't block aligned, then the end of the blob is being paged in. In the compact
    // blob layout, the Merkle tree can share the last block with the data and may have been read
    // into the transfer buffer. The Merkle tree needs to be removed before transferring the pages
    // to the destination VMO. The pages are supplied at page granularity (not block) so only the
    // end of the last page needs to be zeroed.
    ZX_DEBUG_ASSERT(kBlobfsBlockSize % zx_system_get_page_size() == 0);
    if (page_aligned_length > length) {
      memset(static_cast<uint8_t*>(uncompressed_transfer_buffer_mapper_.start()) + length, 0,
             page_aligned_length - length);
    }

    // Verify the pages read in.
    if (zx_status_t status = info.verifier->VerifyPartial(
            uncompressed_transfer_buffer_mapper_.start(), length, offset, page_aligned_length);
        status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "TransferUncompressed: Failed to verify data for blob "
                              << info.verifier->digest();
      return ToPagerErrorStatus(status);
    }

    ZX_DEBUG_ASSERT(offset % zx_system_get_page_size() == 0);
    // Move the pages from the transfer buffer to the destination VMO.
    if (auto status =
            page_supplier(offset, page_aligned_length, uncompressed_transfer_buffer_->GetVmo(), 0);
        status.is_error()) {
      FX_LOGS(ERROR) << "TransferUncompressed: Failed to supply pages to paged VMO for blob "
                     << info.verifier->digest() << ": " << status.status_string();
      return ToPagerErrorStatus(status.error_value());
    }

    // Most of the pages were transferred out of the transfer buffer. Only the pages that weren't
    // transferred need to be decommitted.
    decommit.cancel();
    if (page_aligned_length != block_aligned_length) {
      VmoDecommitter(uncompressed_transfer_buffer_->GetVmo(), page_aligned_length,
                     block_aligned_length - page_aligned_length)();
    }

    length_remaining -= length;
    offset += length;
  }

  fbl::String merkle_root_hash = info.verifier->digest().ToString();
  metrics_->IncrementPageIn(merkle_root_hash, start_offset, total_length);

  return PagerErrorStatus::kOK;
}

// The requested range is aligned in multiple steps as follows:
// 1. The desired uncompressed range is aligned for Merkle tree verification.
// 2. This range is extended to span complete compression frames / chunks, since that is the
// granularity we can decompress data in. The result of this alignment produces a
// CompressionMapping, which contains the mapping of the requested uncompressed range to the
// compressed range that needs to be read in from disk.
// 3. The uncompressed range is processed in chunks equal to the decompression_buffer_size_. For
// each chunk, we compute the CompressionMapping to determine the compressed range that needs to be
// read in. Each chunk is uncompressed and verified as it is read in, and spliced into the
// destination VMO with supply_pages().
//
// There are two assumptions we make here: First that the decompression buffer is sized per the
// alignment requirements for Merkle tree verification. And second that the transfer buffer is sized
// such that it can accommodate all the compressed data for the decompression buffer, i.e. the
// transfer buffer should work with the worst case compression ratio of 1. We have static asserts in
// place to check both these assumptions - the transfer buffer is the same size as the decompression
// buffer (256MB), and both these buffers are 8k block aligned.
PagerErrorStatus PageLoader::Worker::TransferChunkedPages(
    const PageLoader::PageSupplier& page_supplier, uint64_t requested_offset,
    uint64_t requested_length, const LoaderInfo& info) {
  ZX_DEBUG_ASSERT(info.decompressor);

  const auto [offset, length] = GetBlockAlignedReadRange(info, requested_offset, requested_length);

  TRACE_DURATION("blobfs", "PageLoader::TransferChunkedPages", "offset", offset, "length", length);

  fbl::String merkle_root_hash = info.verifier->digest().ToString();

  size_t current_decompressed_offset = offset;
  size_t desired_decompressed_end = offset + length;

  // Read in multiples of the decompression buffer size. In practice we should only require one
  // iteration for the majority of cases, since the decompression buffer is 256MB.
  while (current_decompressed_offset < desired_decompressed_end) {
    size_t current_decompressed_length = desired_decompressed_end - current_decompressed_offset;
    zx::result<CompressionMapping> mapping_status = info.decompressor->MappingForDecompressedRange(
        current_decompressed_offset, current_decompressed_length, decompression_buffer_size_);

    if (!mapping_status.is_ok()) {
      FX_LOGS(ERROR) << "TransferChunked: Failed to find range for [" << offset << ", "
                     << current_decompressed_offset + current_decompressed_length << ") for blob "
                     << info.verifier->digest() << ": " << mapping_status.status_string();
      return ToPagerErrorStatus(mapping_status.status_value());
    }
    CompressionMapping mapping = mapping_status.value();

    // The compressed frame may not fall at a block aligned address, but we read in block aligned
    // chunks. This offset will be applied to the buffer we pass to decompression.
    // TODO(jfsulliv): Caching blocks which span frames may be useful for performance.
    size_t offset_of_compressed_data = mapping.compressed_offset % kBlobfsBlockSize;

    // Read from storage into the transfer buffer.
    size_t read_offset = fbl::round_down(mapping.compressed_offset, kBlobfsBlockSize);
    size_t read_len = (mapping.compressed_length + offset_of_compressed_data);

    // Decommit pages in the transfer buffer that might have been populated. All blobs share the
    // same transfer buffer - this prevents data leaks between different blobs.
    auto decommit_compressed = fit::defer(VmoDecommitter(
        compressed_transfer_buffer_->GetVmo(), 0, fbl::round_up(read_len, kBlobfsBlockSize)));

    auto populate_status = compressed_transfer_buffer_->Populate(read_offset, read_len, info);
    if (!populate_status.is_ok()) {
      FX_LOGS(ERROR) << "TransferChunked: Failed to populate transfer vmo for blob "
                     << info.verifier->digest() << ": " << populate_status.status_string()
                     << ". Returning as plain IO error.";
      return PagerErrorStatus::kErrIO;
    }

    fs::Ticker ticker;
    const uint64_t page_aligned_decompressed_length =
        fbl::round_up<uint64_t, uint64_t>(mapping.decompressed_length, zx_system_get_page_size());
    ZX_DEBUG_ASSERT(sandbox_buffer_.is_valid());
    // Decommit pages in the sandbox buffer that might have been populated. All blobs share
    // the same sandbox buffer - this prevents data leaks between different blobs.
    auto decommit_sandbox =
        fit::defer(VmoDecommitter(sandbox_buffer_, 0, page_aligned_decompressed_length));
    ExternalSeekableDecompressor decompressor(decompressor_client_.get(),
                                              info.decompressor->algorithm());
    if (zx_status_t status = decompressor.DecompressRange(
            offset_of_compressed_data, mapping.compressed_length, mapping.decompressed_length);
        status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "TransferChunked: Failed to decompress for blob "
                              << info.verifier->digest();
      return ToPagerErrorStatus(status);
    }
    metrics_->paged_read_metrics().IncrementDecompression(
        CompressionAlgorithm::kChunked, mapping.decompressed_length, ticker.End());

    // Decommit pages in the decompression buffer that might have been populated. All blobs share
    // the same transfer buffer - this prevents data leaks between different blobs.
    auto decommit_decompressed =
        fit::defer(VmoDecommitter(decompression_buffer_, 0, page_aligned_decompressed_length));
    if (zx_status_t status = decompression_buffer_.transfer_data(
            0, 0, page_aligned_decompressed_length, &sandbox_buffer_, 0);
        status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "TransferChunked: Failed to transfer from sandbox buffer";
      return ToPagerErrorStatus(status);
    }
    // All of the pages were transferred out of the sandbox buffer, they no longer need to be
    // decommitted.
    decommit_sandbox.cancel();

    // The VMO is already mapped but the pages may not be in the page table. Add all of the pages to
    // the page table at once.
    PopulatePageTable(decompression_buffer_mapper_, page_aligned_decompressed_length);

    // Verify the decompressed pages.
    zx_status_t status = info.verifier->VerifyPartial(
        decompression_buffer_mapper_.start(), mapping.decompressed_length,
        mapping.decompressed_offset, page_aligned_decompressed_length);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "TransferChunked: Failed to verify data for blob "
                              << info.verifier->digest();
      return ToPagerErrorStatus(status);
    }

    // Move the pages from the decompression buffer to the destination VMO.
    if (auto status = page_supplier(mapping.decompressed_offset, page_aligned_decompressed_length,
                                    decompression_buffer_, 0);
        status.is_error()) {
      FX_LOGS(ERROR) << "TransferChunked: Failed to supply pages to paged VMO for blob "
                     << info.verifier->digest() << ": " << status.status_string();
      return ToPagerErrorStatus(status.error_value());
    }
    // All of the pages were transferred out of the decompressed buffer, they no longer need to be
    // decommitted.
    decommit_decompressed.cancel();
    metrics_->IncrementPageIn(merkle_root_hash, read_offset, read_len);

    // Advance the required decompressed offset based on how much has already been populated.
    current_decompressed_offset = mapping.decompressed_offset + mapping.decompressed_length;
  }

  return PagerErrorStatus::kOK;
}

PageLoader::PageLoader(std::vector<std::unique_ptr<Worker>> workers)
    : workers_(std::move(workers)) {}

zx::result<std::unique_ptr<PageLoader>> PageLoader::Create(
    std::vector<std::unique_ptr<WorkerResources>> resources, size_t decompression_buffer_size,
    BlobfsMetrics* metrics, DecompressorCreatorConnector* decompression_connector) {
  std::vector<std::unique_ptr<PageLoader::Worker>> workers;
  ZX_ASSERT(!resources.empty());
  for (auto& res : resources) {
    auto worker_or = PageLoader::Worker::Create(std::move(res), decompression_buffer_size, metrics,
                                                decompression_connector);
    if (worker_or.is_error()) {
      return worker_or.take_error();
    }
    workers.push_back(std::move(worker_or.value()));
  }

  auto pager = std::unique_ptr<PageLoader>(new PageLoader(std::move(workers)));

  // Initialize and start the watchdog.
  pager->watchdog_ = fs_watchdog::CreateWatchdog();
  zx::result<> watchdog_status = pager->watchdog_->Start();
  if (!watchdog_status.is_ok()) {
    FX_LOGS(ERROR) << "Could not start pager watchdog";
    return zx::error(watchdog_status.status_value());
  }

  return zx::ok(std::move(pager));
}

uint32_t PageLoader::AllocateWorker() {
  std::lock_guard l(worker_allocation_lock_);
  ZX_DEBUG_ASSERT(worker_id_allocator_ < workers_.size());
  return worker_id_allocator_++;
}

PagerErrorStatus PageLoader::TransferPages(const PageSupplier& page_supplier, uint64_t offset,
                                           uint64_t length, const LoaderInfo& info) {
  static const fs_watchdog::FsOperationType kOperation(
      fs_watchdog::FsOperationType::CommonFsOperation::PageFault, std::chrono::seconds(60));
  [[maybe_unused]] fs_watchdog::FsOperationTracker tracker(&kOperation, watchdog_.get());

  // Assigns a worker to each pager thread statically.
  thread_local uint32_t worker_id = AllocateWorker();
  return workers_[worker_id]->TransferPages(page_supplier, offset, length, info);
}

}  // namespace blobfs
