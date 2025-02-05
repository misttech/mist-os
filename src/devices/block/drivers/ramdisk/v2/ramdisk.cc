// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ramdisk.h"

#include <fidl/fuchsia.hardware.block.volume/cpp/fidl.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <atomic>
#include <memory>
#include <mutex>
#include <random>

#include "zircon/errors.h"

namespace ramdisk_v2 {

// Returns an observer that simply destroy's the dispatcher.  Once registered
// successfully, the caller must call `release` on the unique_ptr since the
// callback takes ownership of the observer.
static std::unique_ptr<fdf_dispatcher_shutdown_observer_t> NewObserver() {
  return std::make_unique<fdf_dispatcher_shutdown_observer_t>(fdf_dispatcher_shutdown_observer_t{
      .handler = [](fdf_dispatcher_t* dispatcher, fdf_dispatcher_shutdown_observer_t* observer) {
        fdf_dispatcher_destroy(dispatcher);
        delete observer;
      }});
}

zx::result<std::unique_ptr<Ramdisk>> Ramdisk::Create(
    RamdiskController* controller, async_dispatcher_t* dispatcher, zx::vmo vmo,
    const block_server::PartitionInfo& partition_info, component::OutgoingDirectory outgoing,
    int id, bool publish) {
  fzl::OwnedVmoMapper mapping;
  if (zx_status_t status =
          mapping.Map(std::move(vmo), 0, partition_info.block_size * partition_info.block_count,
                      ZX_VM_PERM_READ | ZX_VM_PERM_WRITE);
      status != ZX_OK) {
    return zx::error(status);
  }
  std::unique_ptr<Ramdisk> ramdisk(
      new Ramdisk(controller, std::move(mapping), partition_info, std::move(outgoing)));
  if (zx::result result =
          ramdisk->outgoing_.AddUnmanagedProtocol<fuchsia_hardware_ramdisk::Ramdisk>(
              ramdisk->bind_handler(dispatcher));
      result.is_error()) {
    return result.take_error();
  }
  if (zx::result result =
          ramdisk->outgoing_.AddUnmanagedProtocol<fuchsia_hardware_block_volume::Volume>(
              [ramdisk = ramdisk.get()](
                  fidl::ServerEnd<fuchsia_hardware_block_volume::Volume> server_end) {
                ramdisk->block_server_.Serve(std::move(server_end));
              });
      result.is_error()) {
    return result.take_error();
  }

  if (publish) {
    if (auto result = controller->outgoing()->AddService<fuchsia_hardware_block_volume::Service>(
            fuchsia_hardware_block_volume::Service::InstanceHandler({
                .volume =
                    [ramdisk = ramdisk.get()](
                        fidl::ServerEnd<fuchsia_hardware_block_volume::Volume> server_end) {
                      ramdisk->block_server_.Serve(std::move(server_end));
                    },
            }),
            std::to_string(id));
        result.is_error()) {
      FDF_LOGL(ERROR, controller->logger(), "Failed to add service: %s", result.status_string());
      return result.take_error();
    }
  }

  return zx::ok(std::move(ramdisk));
}

Ramdisk::~Ramdisk() {
  {
    std::lock_guard<std::mutex> lock(lock_);
    pre_sleep_write_block_count_ = std::nullopt;
  }
  condition_.notify_all();
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
      FDF_LOGL(INFO, controller_->logger(), "Discarded blocks: %lu",
               blocks_written_since_last_flush_.size());
      blocks_written_since_last_flush_.clear();
    }

    memset(&block_counts_, 0, sizeof(block_counts_));
    pre_sleep_write_block_count_ = std::nullopt;
  }
  condition_.notify_all();
  completer.Reply();
}

void Ramdisk::SleepAfter(SleepAfterRequestView request, SleepAfterCompleter::Sync& completer) {
  {
    std::lock_guard<std::mutex> lock(lock_);
    memset(&block_counts_, 0, sizeof(block_counts_));
    pre_sleep_write_block_count_ = request->count;
  }
  completer.Reply();
}

void Ramdisk::GetBlockCounts(GetBlockCountsCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(lock_);
  completer.Reply(block_counts_);
}

void Ramdisk::StartThread(block_server::Thread thread) {
  constexpr std::string_view dispatcher_name = "Block Server";
  fdf_dispatcher_t* dispatcher;
  auto shutdown_observer = NewObserver();
  zx_status_t status = fdf_dispatcher_create(
      FDF_DISPATCHER_OPTION_SYNCHRONIZED | FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS,
      dispatcher_name.data(), dispatcher_name.size(), nullptr, 0, shutdown_observer.get(),
      &dispatcher);

  if (status == ZX_OK) {
    shutdown_observer.release();
    async::PostTask(fdf_dispatcher_get_async_dispatcher(dispatcher),
                    [thread = std::move(thread), dispatcher]() mutable {
                      thread.Run();
                      fdf_dispatcher_shutdown_async(dispatcher);
                    });
  }
}

void Ramdisk::OnNewSession(block_server::Session session) {
  constexpr std::string_view dispatcher_name = "Block Server Session";
  fdf_dispatcher_t* dispatcher;
  auto shutdown_observer = NewObserver();
  zx_status_t status = fdf_dispatcher_create(
      FDF_DISPATCHER_OPTION_SYNCHRONIZED | FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS,
      dispatcher_name.data(), dispatcher_name.size(), nullptr, 0, shutdown_observer.get(),
      &dispatcher);
  if (status == ZX_OK) {
    shutdown_observer.release();
    async::PostTask(fdf_dispatcher_get_async_dispatcher(dispatcher),
                    [session = std::move(session), dispatcher]() mutable {
                      session.Run();
                      fdf_dispatcher_shutdown_async(dispatcher);
                    });
  }
}

void Ramdisk::OnRequests(const block_server::Session& session,
                         cpp20::span<block_server::Request> requests) TA_NO_THREAD_SAFETY_ANALYSIS {
  for (const auto& request : requests) {
    zx_status_t status = ZX_OK;

    switch (request.operation.tag) {
      case block_server::Operation::Tag::Read:
      case block_server::Operation::Tag::Write: {
        uint64_t block_count = request.operation.read.block_count;
        if (block_count == 0 || request.operation.read.device_block_offset >= block_count_ ||
            block_count_ - request.operation.read.device_block_offset < block_count) {
          status = ZX_ERR_OUT_OF_RANGE;
          break;
        }

        // We might need to split the transaction if we need to sleep in the middle of one.
        uint64_t pre_sleep_count = 0;
        {
          std::unique_lock<std::mutex> lock(lock_);
          if (request.operation.tag == block_server::Operation::Tag::Write) {
            block_counts_.received += block_count;
          }
          if (pre_sleep_write_block_count_ && block_count >= *pre_sleep_write_block_count_) {
            pre_sleep_count = *pre_sleep_write_block_count_;
          }
        }
        if (pre_sleep_count > 0) {
          status = ReadWrite(request, 0, pre_sleep_count);
          block_count -= pre_sleep_count;
          if (status != ZX_OK) {
            std::lock_guard<std::mutex> lock(lock_);
            block_counts_.failed += block_count;
            break;
          }
        }
        {
          std::unique_lock<std::mutex> lock(lock_);
          condition_.wait(lock, [this]() TA_REQ(lock_) {
            return !pre_sleep_write_block_count_ || *pre_sleep_write_block_count_ > 0 ||
                   !static_cast<bool>(flags_ &
                                      fuchsia_hardware_ramdisk::wire::RamdiskFlag::kResumeOnWake);
          });
          if (ShouldFailRequests()) {
            // Fail the requests if we're asleep and `kResumeOnWake` isn't set.
            status = ZX_ERR_UNAVAILABLE;
            block_counts_.failed += block_count;
            break;
          }
        }
        if (block_count > 0)
          status = ReadWrite(request, pre_sleep_count, block_count);
      } break;
      case block_server::Operation::Tag::Flush: {
        std::lock_guard<std::mutex> lock(lock_);
        if (ShouldFailRequests()) {
          status = ZX_ERR_UNAVAILABLE;
        } else {
          blocks_written_since_last_flush_.clear();
        }
      } break;
      case block_server::Operation::Tag::Trim:
        session.SendReply(request.request_id, request.trace_flow_id,
                          zx::error(ZX_ERR_NOT_SUPPORTED));
        break;
      case block_server::Operation::Tag::CloseVmo:
        ZX_PANIC("Unexpected operation");
    }
    session.SendReply(request.request_id, request.trace_flow_id, zx::make_result(status));
  }
}

zx_status_t Ramdisk::ReadWrite(const block_server::Request& request, uint64_t request_block_offset,
                               uint64_t block_count) {
  const bool is_write = request.operation.tag == block_server::Operation::Tag::Write;
  const uint64_t block_offset = request.operation.read.device_block_offset + request_block_offset;
  const uint64_t dev_offset = block_offset * block_size_;
  const uint64_t length = block_count * block_size_;
  const uint64_t vmo_offset =
      request.operation.read.vmo_offset + request_block_offset * block_size_;
  void* addr = reinterpret_cast<void*>(reinterpret_cast<uintptr_t>(mapping_.start()) + dev_offset);

  zx_status_t status = ZX_OK;
  if (is_write) {
    status = request.vmo->read(addr, vmo_offset, length);

    // Update the ramdisk block counts. Since we aren't failing read transactions, only include
    // write transaction counts.
    std::lock_guard<std::mutex> lock(lock_);
    // Increment the count based on the result of the last transaction.
    if (status == ZX_OK) {
      block_counts_.successful += block_count;
      if (pre_sleep_write_block_count_) {
        if (block_count >= *pre_sleep_write_block_count_) {
          *pre_sleep_write_block_count_ = 0;
        } else {
          *pre_sleep_write_block_count_ -= block_count;
        }
        if (!request.operation.write.options.is_force_access() &&
            flags_ & fuchsia_hardware_ramdisk::wire::RamdiskFlag::kDiscardNotFlushedOnWake) {
          std::random_device random;
          std::bernoulli_distribution distribution;
          for (uint64_t block = block_offset, count = block_count; count > 0; ++block, --count) {
            if (!(flags_ & fuchsia_hardware_ramdisk::wire::RamdiskFlag::kDiscardRandom) ||
                distribution(random)) {
              blocks_written_since_last_flush_.push_back(block);
            }
          }
        }
      }
    } else {
      block_counts_.failed += block_count;
    }
  } else {
    status = request.vmo->write(addr, vmo_offset, length);
  }

  return status;
}

bool Ramdisk::ShouldFailRequests() const {
  return pre_sleep_write_block_count_ && *pre_sleep_write_block_count_ == 0 &&
         !static_cast<bool>(flags_ & fuchsia_hardware_ramdisk::wire::RamdiskFlag::kResumeOnWake);
}

}  // namespace ramdisk_v2
