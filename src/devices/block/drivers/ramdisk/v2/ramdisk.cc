// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ramdisk.h"

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
    fdf::DriverBase* controller, async_dispatcher_t* dispatcher, zx::vmo vmo,
    const block_server::PartitionInfo& partition_info, component::OutgoingDirectory outgoing) {
  fzl::OwnedVmoMapper mapping;
  if (zx_status_t status =
          mapping.Map(std::move(vmo), partition_info.block_size * partition_info.block_count);
      status != ZX_OK) {
    ZX_PANIC("5");
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
  return zx::ok(std::move(ramdisk));
}

void Ramdisk::SetFlags(SetFlagsRequestView request, SetFlagsCompleter::Sync& completer) {
  {
    std::lock_guard<std::mutex> lock(lock_);
    flags_ = request->flags;
  }
  completer.Reply();
}

void Ramdisk::Wake(WakeCompleter::Sync& completer) {
  FDF_LOGL(ERROR, controller_->logger(), "Wake not supported");
  completer.Reply();
}

void Ramdisk::SleepAfter(SleepAfterRequestView request, SleepAfterCompleter::Sync& completer) {
  FDF_LOGL(ERROR, controller_->logger(), "SleepAfter not supported");
  completer.Reply();
}

void Ramdisk::GetBlockCounts(GetBlockCountsCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(lock_);
  completer.Reply(block_counts_);
}

void Ramdisk::Grow(GrowRequestView request, GrowCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
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

void Ramdisk::OnRequests(block_server::Session& session,
                         cpp20::span<const block_server::Request> requests) {
  for (const auto& request : requests) {
    if (request.operation.tag == block_server::Operation::Tag::Flush) {
      session.SendReply(request.request_id, zx::ok());
      continue;
    } else if (request.operation.tag == block_server::Operation::Tag::Trim) {
      session.SendReply(request.request_id, zx::error(ZX_ERR_NOT_SUPPORTED));
      continue;
    }

    const bool is_write = request.operation.tag == block_server::Operation::Tag::Write;
    uint64_t blocks = request.operation.read.block_count;
    const uint64_t block_offset = request.operation.read.device_block_offset;
    if (blocks == 0 || block_offset >= block_count_ || block_count_ - block_offset < blocks) {
      session.SendReply(request.request_id, zx::error(ZX_ERR_OUT_OF_RANGE));
      continue;
    }

    const uint64_t dev_offset = block_offset * block_size_;
    const uint64_t length = blocks * block_size_;
    const uint64_t vmo_offset = request.operation.read.vmo_offset;
    void* addr =
        reinterpret_cast<void*>(reinterpret_cast<uintptr_t>(mapping_.start()) + dev_offset);

    zx_status_t status = ZX_OK;
    if (is_write) {
      if (length > 0) {
        status = request.vmo->read(addr, vmo_offset, length);
      }

      // Update the ramdisk block counts. Since we aren't failing read transactions, only include
      // write transaction counts.
      std::lock_guard<std::mutex> lock(lock_);
      // Increment the count based on the result of the last transaction.
      if (status == ZX_OK) {
        block_counts_.successful += blocks;
      } else {
        block_counts_.failed += blocks;
      }
    } else {
      status = request.vmo->write(addr, vmo_offset, length);
    }

    session.SendReply(request.request_id, zx::make_result(status));
  }
}

}  // namespace ramdisk_v2
