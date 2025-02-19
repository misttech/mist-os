// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ramdisk.h"

#include <inttypes.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/fzl/owned-vmo-mapper.h>
#include <lib/zbi-format/partition.h>
#include <lib/zx/vmo.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <atomic>
#include <limits>
#include <memory>
#include <mutex>
#include <random>

#include "src/devices/block/lib/common/include/common-dfv1.h"
#include "zircon/errors.h"

namespace ramdisk {
namespace {

using Transaction = block::BorrowedOperation<>;

constexpr uint64_t kMaxTransferSize = 1LLU << 19;

static std::atomic<uint64_t> g_ramdisk_count = 0;

}  // namespace

Ramdisk::Ramdisk(zx_device_t* parent, uint64_t block_size, uint64_t block_count,
                 const uint8_t* type_guid, fzl::OwnedVmoMapper mapping)
    : RamdiskDeviceType(parent),
      block_size_(block_size),
      block_count_(block_count),
      mapping_(std::move(mapping)) {
  if (type_guid) {
    memcpy(type_guid_, type_guid, ZBI_PARTITION_GUID_LEN);
  } else {
    memset(type_guid_, 0, ZBI_PARTITION_GUID_LEN);
  }
  snprintf(name_, sizeof(name_), "ramdisk-%" PRIu64, g_ramdisk_count.fetch_add(1));
}

zx_status_t Ramdisk::Create(zx_device_t* parent, zx::vmo vmo, uint64_t block_size,
                            uint64_t block_count, const uint8_t* type_guid,
                            std::unique_ptr<Ramdisk>* out) {
  fzl::OwnedVmoMapper mapping;
  zx_status_t status =
      mapping.Map(std::move(vmo), 0, block_size * block_count, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE);
  if (status != ZX_OK) {
    return status;
  }

  auto ramdev = std::unique_ptr<Ramdisk>(
      new Ramdisk(parent, block_size, block_count, type_guid, std::move(mapping)));
  if (thrd_create(&ramdev->worker_, WorkerThunk, ramdev.get()) != thrd_success) {
    return ZX_ERR_NO_MEMORY;
  }

  *out = std::move(ramdev);
  return ZX_OK;
}

zx_status_t Ramdisk::DdkGetProtocol(uint32_t proto_id, void* out_protocol) {
  auto* proto = static_cast<ddk::AnyProtocol*>(out_protocol);
  proto->ctx = this;
  switch (proto_id) {
    case ZX_PROTOCOL_BLOCK_IMPL: {
      proto->ops = &block_impl_protocol_ops_;
      return ZX_OK;
    }
    case ZX_PROTOCOL_BLOCK_PARTITION: {
      proto->ops = &block_partition_protocol_ops_;
      return ZX_OK;
    }
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

void Ramdisk::DdkUnbind(ddk::UnbindTxn txn) {
  {
    std::lock_guard<std::mutex> lock(lock_);
    dead_ = true;
  }
  sync_completion_signal(&signal_);
  txn.Reply();
}

void Ramdisk::DdkRelease() {
  {
    // Idempotent, so if this has already been triggered earlier it is a no-op.
    std::lock_guard<std::mutex> lock(lock_);
    dead_ = true;
  }
  // Wake up the worker thread, in case it is sleeping
  sync_completion_signal(&signal_);

  thrd_join(worker_, nullptr);
  delete this;
}

void Ramdisk::BlockImplQuery(block_info_t* info, size_t* bopsz) {
  memset(info, 0, sizeof(*info));
  info->block_size = static_cast<uint32_t>(block_size_);
  info->block_count = block_count_;
  // Arbitrarily set, but matches the SATA driver for testing
  info->max_transfer_size = kMaxTransferSize;
  // None of the block flags are applied to the ramdisk.
  info->flags = 0;
  *bopsz = Transaction::OperationSize(sizeof(block_op_t));
}

void Ramdisk::BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb,
                             void* cookie) {
  Transaction txn(bop, completion_cb, cookie, sizeof(block_op_t));
  bool dead;
  bool read = false;

  const auto flags = txn.operation()->command.flags;
  switch (txn.operation()->command.opcode) {
    case BLOCK_OPCODE_READ: {
      read = true;
      __FALLTHROUGH;
    }
    case BLOCK_OPCODE_WRITE: {
      if (zx_status_t status = block::CheckIoRange(txn.operation()->rw, block_count_);
          status != ZX_OK) {
        txn.Complete(status);
        return;
      }
      if (flags & BLOCK_IO_FLAG_FORCE_ACCESS) {
        txn.Complete(ZX_ERR_NOT_SUPPORTED);
        return;
      }

      {
        std::lock_guard<std::mutex> lock(lock_);
        if (!(dead = dead_)) {
          if (!read) {
            block_counts_.received += txn.operation()->rw.length;
          }
          txn_list_.push(std::move(txn));
        }
      }

      if (dead) {
        txn.Complete(ZX_ERR_BAD_STATE);
      } else {
        sync_completion_signal(&signal_);
      }
      break;
    }
    case BLOCK_OPCODE_FLUSH: {
      {
        std::lock_guard<std::mutex> lock(lock_);
        if (!(dead = dead_)) {
          txn_list_.push(std::move(txn));
        }
      }
      if (dead) {
        txn.Complete(ZX_ERR_BAD_STATE);
      } else {
        sync_completion_signal(&signal_);
      }
      break;
    }
    default: {
      txn.Complete(ZX_ERR_NOT_SUPPORTED);
      break;
    }
  }
}

void Ramdisk::SetFlags(SetFlagsRequestView request, SetFlagsCompleter::Sync& completer) {
  {
    std::lock_guard<std::mutex> lock(lock_);
    flags_ = request->flags;
  }
  completer.Reply();
}

void Ramdisk::Wake(WakeCompleter::Sync& completer) {
  {
    std::lock_guard<std::mutex> lock(lock_);

    if (flags_ & fuchsia_hardware_ramdisk::wire::RamdiskFlag::kDiscardNotFlushedOnWake) {
      // Fill all blocks with a fill pattern.
      for (uint64_t block : blocks_written_since_last_flush_) {
        void* addr = reinterpret_cast<void*>(reinterpret_cast<uintptr_t>(mapping_.start()) +
                                             block * block_size_);
        memset(addr, 0xaf, block_size_);
      }
      zxlogf(INFO, "Discarded blocks: %lu", blocks_written_since_last_flush_.size());
      blocks_written_since_last_flush_.clear();
    }

    asleep_ = false;
    memset(&block_counts_, 0, sizeof(block_counts_));
    pre_sleep_write_block_count_ = 0;
    sync_completion_signal(&signal_);
  }
  completer.Reply();
}

void Ramdisk::SleepAfter(SleepAfterRequestView request, SleepAfterCompleter::Sync& completer) {
  {
    std::lock_guard<std::mutex> lock(lock_);
    asleep_ = false;
    memset(&block_counts_, 0, sizeof(block_counts_));
    pre_sleep_write_block_count_ = request->count;

    if (request->count == 0) {
      asleep_ = true;
    }
  }
  completer.Reply();
}

void Ramdisk::GetBlockCounts(GetBlockCountsCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(lock_);
  completer.Reply(block_counts_);
}

zx_status_t Ramdisk::BlockPartitionGetGuid(guidtype_t guid_type, guid_t* out_guid) {
  if (guid_type != GUIDTYPE_TYPE) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  static_assert(ZBI_PARTITION_GUID_LEN == GUID_LENGTH, "GUID length mismatch");
  memcpy(out_guid, type_guid_, ZBI_PARTITION_GUID_LEN);
  return ZX_OK;
}

zx_status_t Ramdisk::BlockPartitionGetName(char* out_name, size_t capacity) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Ramdisk::BlockPartitionGetMetadata(partition_metadata_t* out_metadata) {
  *out_metadata = partition_metadata_t{
      .num_blocks = block_count_,
  };
  memcpy(&out_metadata->type_guid, type_guid_, ZBI_PARTITION_GUID_LEN);
  return ZX_OK;
}

void Ramdisk::ProcessRequests() {
  block::BorrowedOperationQueue<> deferred_list;
  std::random_device random;
  std::bernoulli_distribution distribution;

  for (;;) {
    std::optional<Transaction> txn;
    bool defer;
    uint64_t block_write_limit;

    do {
      {
        std::lock_guard<std::mutex> lock(lock_);
        defer =
            static_cast<bool>(flags_ & fuchsia_hardware_ramdisk::wire::RamdiskFlag::kResumeOnWake);
        block_write_limit = pre_sleep_write_block_count_ == 0 && !asleep_
                                ? std::numeric_limits<uint64_t>::max()
                                : pre_sleep_write_block_count_;

        if (dead_) {
          while ((txn = deferred_list.pop())) {
            txn->Complete(ZX_ERR_BAD_STATE);
          }
          while ((txn = txn_list_.pop())) {
            txn->Complete(ZX_ERR_BAD_STATE);
          }
          return;
        }

        if (!asleep_) {
          // If we are awake, try grabbing pending transactions from the deferred list.
          txn = deferred_list.pop();
        }

        if (!txn) {
          // If no transactions were available in the deferred list (or we are asleep),
          // grab one from the regular txn_list.
          txn = txn_list_.pop();
        }
      }

      if (!txn) {
        sync_completion_wait(&signal_, ZX_TIME_INFINITE);
        sync_completion_reset(&signal_);
      }
    } while (!txn);

    if (txn->operation()->command.opcode == BLOCK_OPCODE_FLUSH) {
      zx_status_t status = ZX_OK;
      if (block_write_limit == 0) {
        status = ZX_ERR_UNAVAILABLE;
      } else {
        std::lock_guard<std::mutex> lock(lock_);
        blocks_written_since_last_flush_.clear();
      }
      txn->Complete(status);
      continue;
    }

    uint32_t blocks = txn->operation()->rw.length;
    if (txn->operation()->command.opcode == BLOCK_OPCODE_WRITE && blocks > block_write_limit) {
      // Limit the number of blocks we write.
      blocks = static_cast<uint32_t>(block_write_limit);
    }
    const uint64_t length = blocks * block_size_;
    const uint64_t dev_offset = txn->operation()->rw.offset_dev * block_size_;
    const uint64_t vmo_offset = txn->operation()->rw.offset_vmo * block_size_;
    void* addr =
        reinterpret_cast<void*>(reinterpret_cast<uintptr_t>(mapping_.start()) + dev_offset);
    auto command = txn->operation()->command;

    zx_status_t status = ZX_OK;
    if (length > kMaxTransferSize) {
      status = ZX_ERR_OUT_OF_RANGE;
    } else if (command.opcode == BLOCK_OPCODE_READ) {
      // A read operation should always succeed, even if the ramdisk is "asleep".
      status = zx_vmo_write(txn->operation()->rw.vmo, addr, vmo_offset, length);
    } else {  // BLOCK_OP_WRITE
      if (length > 0) {
        status = zx_vmo_read(txn->operation()->rw.vmo, addr, vmo_offset, length);
      }

      // Update the ramdisk block counts. Since we aren't failing read transactions, only include
      // write transaction counts.
      std::lock_guard<std::mutex> lock(lock_);
      // Increment the count based on the result of the last transaction.
      if (status == ZX_OK) {
        block_counts_.successful += blocks;

        // Put the ramdisk to sleep if we have reached the required # of blocks. It's possible that
        // an update to the sleep count arrived whilst we didn't hold the lock, so we check for that
        // here. If it has happened, then just don't count this transaction i.e. we pretend that it
        // completed before the update to the sleep count.
        if (pre_sleep_write_block_count_ == block_write_limit) {
          pre_sleep_write_block_count_ -= blocks;
          asleep_ = (pre_sleep_write_block_count_ == 0);

          if (flags_ & fuchsia_hardware_ramdisk::wire::RamdiskFlag::kDiscardNotFlushedOnWake) {
            for (uint64_t block = txn->operation()->rw.offset_dev, count = blocks; count > 0;
                 ++block, --count) {
              if (!(flags_ & fuchsia_hardware_ramdisk::wire::RamdiskFlag::kDiscardRandom) ||
                  distribution(random)) {
                blocks_written_since_last_flush_.push_back(block);
              }
            }
          }
        }

        if (blocks < txn->operation()->rw.length) {
          if (defer) {
            // If the first part of the transaction succeeded but the entire transaction is not
            // complete, we need to address the remainder.

            // If we are deferring after this block count, update the transaction to reflect the
            // blocks that have already been written, and add it to the deferred queue.
            txn->operation()->rw.length -= blocks;
            txn->operation()->rw.offset_vmo += blocks;
            txn->operation()->rw.offset_dev += blocks;

            // Add the remaining blocks to the deferred list.
            deferred_list.push(std::move(*txn));

            // Hold off on returning the result until the remainder of the transaction is completed.
            continue;
          } else {
            block_counts_.failed += txn->operation()->rw.length - blocks;
            status = ZX_ERR_UNAVAILABLE;
          }
        }
      } else {
        block_counts_.failed += txn->operation()->rw.length;
      }
    }

    txn->Complete(status);
  }
}

}  // namespace ramdisk
