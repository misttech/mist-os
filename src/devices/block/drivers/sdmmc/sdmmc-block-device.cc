// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdmmc-block-device.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/fit/defer.h>
#include <lib/fzl/vmo-mapper.h>
#include <threads.h>
#include <zircon/hw/gpt.h>
#include <zircon/process.h>
#include <zircon/status.h>
#include <zircon/threads.h>

#include <fbl/alloc_checker.h>
#include <safemath/safe_conversions.h>

#include "sdmmc-partition-device.h"
#include "sdmmc-root-device.h"
#include "sdmmc-rpmb-device.h"
#include "src/devices/block/lib/common/include/common.h"

namespace {

constexpr size_t kTranMaxAttempts = 10;

// Boot and RPMB partition sizes are in units of 128 KiB/KB.
constexpr uint32_t kBootSizeMultiplier = 128 * 1024;

// Populates and returns a fuchsia_hardware_sdmmc::wire::SdmmcBufferRegion using the supplied
// arguments.
zx::result<fuchsia_hardware_sdmmc::wire::SdmmcBufferRegion> GetBufferRegion(zx_handle_t vmo,
                                                                            uint64_t offset,
                                                                            uint64_t size,
                                                                            fdf::Logger& logger) {
  zx::vmo dup;
  zx_status_t status = zx_handle_duplicate(vmo, ZX_RIGHT_SAME_RIGHTS, dup.reset_and_get_address());
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger, "Failed to duplicate vmo: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  fuchsia_hardware_sdmmc::wire::SdmmcBufferRegion buffer_region;
  buffer_region.buffer = fuchsia_hardware_sdmmc::wire::SdmmcBuffer::WithVmo(std::move(dup));
  buffer_region.type = fuchsia_hardware_sdmmc::wire::SdmmcBufferType::kVmoHandle;
  buffer_region.offset = offset;
  buffer_region.size = size;
  return zx::ok(std::move(buffer_region));
}

}  // namespace

namespace sdmmc {

fdf::Logger& ReadWriteMetadata::logger() { return block_device->logger(); }

void SdmmcBlockDevice::BlockComplete(sdmmc::BlockOperation& txn, zx_status_t status) {
  if (txn.node()->complete_cb()) {
    txn.Complete(status);
  } else {
    FDF_LOGL(DEBUG, logger(), "block op %p completion_cb unset!", txn.operation());
  }
}

zx_status_t SdmmcBlockDevice::Create(SdmmcRootDevice* parent, std::unique_ptr<SdmmcDevice> sdmmc,
                                     std::unique_ptr<SdmmcBlockDevice>* out_dev) {
  fbl::AllocChecker ac;
  out_dev->reset(new (&ac) SdmmcBlockDevice(parent, std::move(sdmmc)));
  if (!ac.check()) {
    FDF_LOGL(ERROR, parent->logger(), "failed to allocate device memory");
    return ZX_ERR_NO_MEMORY;
  }

  return ZX_OK;
}

zx_status_t SdmmcBlockDevice::AddDevice() {
  // Device must be in TRAN state at this point
  zx_status_t st = WaitForTran();
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "waiting for TRAN state failed, retcode = %d", st);
    return ZX_ERR_TIMED_OUT;
  }

  root_ = inspector_.GetRoot().CreateChild("sdmmc_core");
  properties_.io_errors_ = root_.CreateUint("io_errors", 0);
  properties_.io_retries_ = root_.CreateUint("io_retries", 0);

  if (!is_sd_) {
    MmcSetInspectProperties();
  }

  fbl::AutoLock lock(&lock_);

  auto dispatcher = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "sdmmc-block-worker",
      [&](fdf_dispatcher_t*) { worker_shutdown_completion_.Signal(); },
      "fuchsia.devices.block.drivers.sdmmc.worker");
  if (dispatcher.is_error()) {
    FDF_LOGL(ERROR, logger(), "Failed to create dispatcher: %s",
             zx_status_get_string(dispatcher.status_value()));
    return dispatcher.status_value();
  }
  worker_dispatcher_ = *std::move(dispatcher);

  st = async::PostTask(worker_dispatcher_.async_dispatcher(), [this] { WorkerLoop(); });
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to start worker thread: %s", zx_status_get_string(st));
    return st;
  }

  auto inspect_sink = parent_->driver_incoming()->Connect<fuchsia_inspect::InspectSink>();
  if (inspect_sink.is_error() || !inspect_sink->is_valid()) {
    FDF_LOGL(ERROR, logger(), "Failed to connect to inspect sink: %s",
             inspect_sink.status_string());
    return inspect_sink.status_value();
  }
  exposed_inspector_.emplace(inspect::ComponentInspector(
      parent_->driver_async_dispatcher(),
      {.inspector = inspector_, .client_end = std::move(inspect_sink.value())}));

  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
  auto [node_client_end, node_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::Node>::Create();

  controller_.Bind(std::move(controller_client_end));
  block_node_.Bind(std::move(node_client_end));

  fidl::Arena arena;

  block_name_ = is_sd_ ? "sdmmc-sd" : "sdmmc-mmc";
  const auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, block_name_).Build();

  auto result = parent_->root_node()->AddChild(args, std::move(controller_server_end),
                                               std::move(node_server_end));
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to add child block device: %s", result.status_string());
    return result.status();
  }

  auto remove_device_on_error =
      fit::defer([&]() { [[maybe_unused]] auto result = controller_->Remove(); });

  fbl::AllocChecker ac;
  std::unique_ptr<PartitionDevice> user_partition(
      new (&ac) PartitionDevice(this, block_info_, USER_DATA_PARTITION));
  if (!ac.check()) {
    FDF_LOGL(ERROR, logger(), "failed to allocate device memory");
    return ZX_ERR_NO_MEMORY;
  }

  if ((st = user_partition->AddDevice()) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to add user partition device: %d", st);
    return st;
  }

  child_partition_devices_.push_back(std::move(user_partition));

  if (!is_sd_) {
    const uint32_t boot_size = raw_ext_csd_[MMC_EXT_CSD_BOOT_SIZE_MULT] * kBootSizeMultiplier;
    const bool boot_enabled =
        raw_ext_csd_[MMC_EXT_CSD_PARTITION_CONFIG] & MMC_EXT_CSD_BOOT_PARTITION_ENABLE_MASK;
    if (boot_size > 0 && boot_enabled) {
      const uint64_t boot_partition_block_count = boot_size / block_info_.block_size;
      const block_info_t boot_info = {
          .block_count = boot_partition_block_count,
          .block_size = block_info_.block_size,
          .max_transfer_size = block_info_.max_transfer_size,
          .flags = block_info_.flags,
      };

      std::unique_ptr<PartitionDevice> boot_partition_1(
          new (&ac) PartitionDevice(this, boot_info, BOOT_PARTITION_1));
      if (!ac.check()) {
        FDF_LOGL(ERROR, logger(), "failed to allocate device memory");
        return ZX_ERR_NO_MEMORY;
      }

      std::unique_ptr<PartitionDevice> boot_partition_2(
          new (&ac) PartitionDevice(this, boot_info, BOOT_PARTITION_2));
      if (!ac.check()) {
        FDF_LOGL(ERROR, logger(), "failed to allocate device memory");
        return ZX_ERR_NO_MEMORY;
      }

      if ((st = boot_partition_1->AddDevice()) != ZX_OK) {
        FDF_LOGL(ERROR, logger(), "failed to add boot partition device: %d", st);
        return st;
      }

      child_partition_devices_.push_back(std::move(boot_partition_1));

      if ((st = boot_partition_2->AddDevice()) != ZX_OK) {
        FDF_LOGL(ERROR, logger(), "failed to add boot partition device: %d", st);
        return st;
      }

      child_partition_devices_.push_back(std::move(boot_partition_2));
    }
  }

  if (!is_sd_ && raw_ext_csd_[MMC_EXT_CSD_RPMB_SIZE_MULT] > 0) {
    std::unique_ptr<RpmbDevice> rpmb_device(new (&ac) RpmbDevice(this, raw_cid_, raw_ext_csd_));
    if (!ac.check()) {
      FDF_LOGL(ERROR, logger(), "failed to allocate device memory");
      return ZX_ERR_NO_MEMORY;
    }

    if ((st = rpmb_device->AddDevice()) != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "failed to add rpmb device: %d", st);
      return st;
    }

    child_rpmb_device_ = std::move(rpmb_device);
  }

  remove_device_on_error.cancel();
  return ZX_OK;
}

void SdmmcBlockDevice::StopWorkerDispatcher(std::optional<fdf::PrepareStopCompleter> completer) {
  if (worker_dispatcher_.get()) {
    {
      fbl::AutoLock lock(&lock_);
      shutdown_ = true;
      worker_event_.Broadcast();
    }

    worker_dispatcher_.ShutdownAsync();
    worker_shutdown_completion_.Wait();
  }

  // error out all pending requests
  fbl::AutoLock lock(&lock_);
  txn_list_.CompleteAll(ZX_ERR_CANCELED);

  for (auto& request : rpmb_list_) {
    request.completer.ReplyError(ZX_ERR_CANCELED);
  }
  rpmb_list_.clear();

  if (completer.has_value()) {
    completer.value()(zx::ok());
  }
}

zx_status_t SdmmcBlockDevice::ReadWriteWithRetries(std::vector<BlockOperation>& btxns,
                                                   const EmmcPartition partition) {
  zx_status_t st = SetPartition(partition);
  if (st != ZX_OK) {
    return st;
  }

  uint32_t attempts = 0;
  while (true) {
    attempts++;
    const bool last_attempt = attempts >= sdmmc_->kTryAttempts;

    st = ReadWriteAttempt(btxns, !last_attempt);

    if (st == ZX_OK || last_attempt) {
      break;
    }
  }

  properties_.io_retries_.Add(attempts - 1);
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "do_txn error: %s", zx_status_get_string(st));
    properties_.io_errors_.Add(1);
  }

  FDF_LOGL(DEBUG, logger(), "do_txn complete");
  return st;
}

zx_status_t SdmmcBlockDevice::ReadWriteAttempt(std::vector<BlockOperation>& btxns,
                                               bool suppress_error_messages) {
  // For single-block transfers, we could get higher performance by using SDMMC_READ_BLOCK/
  // SDMMC_WRITE_BLOCK without the need to SDMMC_SET_BLOCK_COUNT or SDMMC_STOP_TRANSMISSION.
  // However, we always do multiple-block transfers for simplicity.
  ZX_DEBUG_ASSERT(btxns.size() >= 1);
  const block_read_write_t& txn = btxns[0].operation()->rw;
  const bool is_read = txn.command.opcode == BLOCK_OPCODE_READ;
  const bool command_packing = btxns.size() > 1;
  const uint32_t cmd_idx = is_read ? SDMMC_READ_MULTIPLE_BLOCK : SDMMC_WRITE_MULTIPLE_BLOCK;
  const uint32_t cmd_flags =
      is_read ? SDMMC_READ_MULTIPLE_BLOCK_FLAGS : SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS;
  uint32_t total_data_transfer_blocks = 0;
  for (const auto& btxn : btxns) {
    total_data_transfer_blocks += btxn.operation()->rw.length;
  }

  FDF_LOGL(DEBUG, logger(),
           "sdmmc: do_txn blockop 0x%x offset_vmo 0x%" PRIx64
           " length 0x%x packing_count %zu blocksize 0x%x"
           " max_transfer_size 0x%x",
           txn.command.opcode, txn.offset_vmo, total_data_transfer_blocks, btxns.size(),
           block_info_.block_size, block_info_.max_transfer_size);

  fdf::Arena arena('SDMC');
  fidl::VectorView<fuchsia_hardware_sdmmc::wire::SdmmcReq> reqs;
  if (!command_packing) {
    // TODO(https://fxbug.dev/42076962): Consider using SDMMC_CMD_AUTO23, which is likely to enhance
    // performance.
    reqs.Allocate(arena, 2);

    auto& set_block_count = reqs[0];
    set_block_count.cmd_idx = SDMMC_SET_BLOCK_COUNT;
    set_block_count.cmd_flags = SDMMC_SET_BLOCK_COUNT_FLAGS;
    set_block_count.arg = total_data_transfer_blocks;

    auto& rw_multiple_block = reqs[1];
    rw_multiple_block.cmd_idx = cmd_idx;
    rw_multiple_block.cmd_flags = cmd_flags;
    rw_multiple_block.arg = static_cast<uint32_t>(txn.offset_dev);
    rw_multiple_block.blocksize = block_info_.block_size;
    rw_multiple_block.buffers.Allocate(arena, 1);
    auto buffer_region = GetBufferRegion(txn.vmo, txn.offset_vmo * block_info_.block_size,
                                         txn.length * block_info_.block_size, logger());
    if (buffer_region.is_error()) {
      return buffer_region.status_value();
    }
    rw_multiple_block.buffers[0] = *std::move(buffer_region);
  } else {
    // Form packed command header (section 6.6.29.1, eMMC standard 5.1)
    readwrite_metadata_.packed_command_header_data->rw = is_read ? 1 : 2;
    // Safe because btxns.size() <= kMaxPackedCommandsFor512ByteBlockSize.
    readwrite_metadata_.packed_command_header_data->num_entries =
        safemath::checked_cast<uint8_t>(btxns.size());

    // TODO(https://fxbug.dev/42083080): Consider pre-registering the packed command header VMO with
    // the SDMMC driver to avoid pinning and unpinning for each transfer. Also handle the cache ops
    // here.
    // Packed write: SET_BLOCK_COUNT (header+data) -> WRITE_MULTIPLE_BLOCK (header+data)
    // Packed read: SET_BLOCK_COUNT (header) -> WRITE_MULTIPLE_BLOCK (header) ->
    //              SET_BLOCK_COUNT (data) -> READ_MULTIPLE_BLOCK (data)
    int request_index_offset;
    if (!is_read) {
      request_index_offset = 0;
      reqs.Allocate(arena, 2);
    } else {
      request_index_offset = 2;
      reqs.Allocate(arena, 4);

      auto& set_block_count = reqs[0];
      set_block_count.cmd_idx = SDMMC_SET_BLOCK_COUNT;
      set_block_count.cmd_flags = SDMMC_SET_BLOCK_COUNT_FLAGS;
      set_block_count.arg = MMC_SET_BLOCK_COUNT_PACKED | 1;  // 1 header block.

      auto& write_multiple_block = reqs[1];
      write_multiple_block.cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK;
      write_multiple_block.cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS;
      write_multiple_block.arg = static_cast<uint32_t>(txn.offset_dev);
      write_multiple_block.blocksize = block_info_.block_size;
      write_multiple_block.buffers.Allocate(arena, 1);  // 1 header block.
      // The first buffer region points to the header (packed read case).
      auto buffer_region = GetBufferRegion(readwrite_metadata_.packed_command_header_vmo.get(), 0,
                                           block_info_.block_size, logger());
      if (buffer_region.is_error()) {
        return buffer_region.status_value();
      }
      write_multiple_block.buffers[0] = *std::move(buffer_region);
    }

    auto& set_block_count = reqs[request_index_offset];
    set_block_count.cmd_idx = SDMMC_SET_BLOCK_COUNT;
    set_block_count.cmd_flags = SDMMC_SET_BLOCK_COUNT_FLAGS;
    set_block_count.arg = MMC_SET_BLOCK_COUNT_PACKED |
                          (is_read ? total_data_transfer_blocks
                                   : (total_data_transfer_blocks + 1));  // +1 for header block.

    auto& rw_multiple_block = reqs[request_index_offset + 1];
    rw_multiple_block.cmd_idx = cmd_idx;
    rw_multiple_block.cmd_flags = cmd_flags;
    rw_multiple_block.arg = static_cast<uint32_t>(txn.offset_dev);
    rw_multiple_block.blocksize = block_info_.block_size;

    int buffer_index_offset;
    if (is_read) {
      buffer_index_offset = 0;
      rw_multiple_block.buffers.Allocate(arena, btxns.size());
    } else {
      buffer_index_offset = 1;
      rw_multiple_block.buffers.Allocate(arena, btxns.size() + 1);  // +1 for header block.
      // The first buffer region points to the header (packed write case).
      auto buffer_region = GetBufferRegion(readwrite_metadata_.packed_command_header_vmo.get(), 0,
                                           block_info_.block_size, logger());
      if (buffer_region.is_error()) {
        return buffer_region.status_value();
      }
      rw_multiple_block.buffers[0] = *std::move(buffer_region);
    }

    // The following buffer regions point to the data.
    for (size_t i = 0; i < btxns.size(); i++) {
      const block_read_write_t& rw = btxns[i].operation()->rw;
      readwrite_metadata_.packed_command_header_data->arg[i].cmd23_arg = rw.length;
      readwrite_metadata_.packed_command_header_data->arg[i].cmdXX_arg =
          static_cast<uint32_t>(rw.offset_dev);

      auto buffer_region = GetBufferRegion(rw.vmo, rw.offset_vmo * block_info_.block_size,
                                           rw.length * block_info_.block_size, logger());
      if (buffer_region.is_error()) {
        return buffer_region.status_value();
      }
      rw_multiple_block.buffers[buffer_index_offset + i] = *std::move(buffer_region);
    }
  }

  for (auto& req : reqs) {
    req.suppress_error_messages = suppress_error_messages;
  }
  return sdmmc_->SdmmcIoRequest(std::move(arena), reqs, readwrite_metadata_.buffer_regions.get());
}

zx_status_t SdmmcBlockDevice::Flush() {
  if (!cache_enabled_) {
    return ZX_OK;
  }

  // TODO(https://fxbug.dev/42075502): Enable the cache and add flush support for SD.
  ZX_ASSERT(!is_sd_);

  zx_status_t st = MmcDoSwitch(MMC_EXT_CSD_FLUSH_CACHE, MMC_EXT_CSD_FLUSH_MASK);
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to flush the cache: %s", zx_status_get_string(st));
  }
  return st;
}

zx_status_t SdmmcBlockDevice::Trim(const block_trim_t& txn, const EmmcPartition partition) {
  // TODO(b/312236221): Add trim support for SD.
  if (is_sd_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (!(block_info_.flags & FLAG_TRIM_SUPPORT)) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t status = SetPartition(partition);
  if (status != ZX_OK) {
    return status;
  }

  constexpr uint32_t kEraseErrorFlags =
      MMC_STATUS_ADDR_OUT_OF_RANGE | MMC_STATUS_ERASE_SEQ_ERR | MMC_STATUS_ERASE_PARAM;

  const sdmmc_req_t trim_start = {
      .cmd_idx = MMC_ERASE_GROUP_START,
      .cmd_flags = MMC_ERASE_GROUP_START_FLAGS,
      .arg = static_cast<uint32_t>(txn.offset_dev),
  };
  uint32_t response[4] = {};
  if ((status = sdmmc_->Request(&trim_start, response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to set trim group start: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }
  if (response[0] & kEraseErrorFlags) {
    FDF_LOGL(ERROR, logger(), "card reported trim group start error: 0x%08x", response[0]);
    properties_.io_errors_.Add(1);
    return ZX_ERR_IO;
  }

  const sdmmc_req_t trim_end = {
      .cmd_idx = MMC_ERASE_GROUP_END,
      .cmd_flags = MMC_ERASE_GROUP_END_FLAGS,
      .arg = static_cast<uint32_t>(txn.offset_dev + txn.length - 1),
  };
  if ((status = sdmmc_->Request(&trim_end, response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to set trim group end: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }
  if (response[0] & kEraseErrorFlags) {
    FDF_LOGL(ERROR, logger(), "card reported trim group end error: 0x%08x", response[0]);
    properties_.io_errors_.Add(1);
    return ZX_ERR_IO;
  }

  const sdmmc_req_t trim = {
      .cmd_idx = SDMMC_ERASE,
      .cmd_flags = SDMMC_ERASE_FLAGS,
      .arg = MMC_ERASE_TRIM_ARG,
  };
  if ((status = sdmmc_->Request(&trim, response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "trim failed: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }
  if (response[0] & kEraseErrorFlags) {
    FDF_LOGL(ERROR, logger(), "card reported trim error: 0x%08x", response[0]);
    properties_.io_errors_.Add(1);
    return ZX_ERR_IO;
  }

  return ZX_OK;
}

zx_status_t SdmmcBlockDevice::RpmbRequest(const RpmbRequestInfo& request) {
  // TODO(https://fxbug.dev/42166356): Find out if RPMB requests can be retried.
  using fuchsia_hardware_rpmb::wire::kFrameSize;

  const uint64_t tx_frame_count = request.tx_frames.size / kFrameSize;
  const uint64_t rx_frame_count =
      request.rx_frames.vmo.is_valid() ? (request.rx_frames.size / kFrameSize) : 0;
  const bool read_needed = rx_frame_count > 0;

  zx_status_t status = SetPartition(RPMB_PARTITION);
  if (status != ZX_OK) {
    return status;
  }

  const sdmmc_req_t set_tx_block_count = {
      .cmd_idx = SDMMC_SET_BLOCK_COUNT,
      .cmd_flags = SDMMC_SET_BLOCK_COUNT_FLAGS,
      .arg = MMC_SET_BLOCK_COUNT_RELIABLE_WRITE | static_cast<uint32_t>(tx_frame_count),
  };
  uint32_t unused_response[4];
  if ((status = sdmmc_->Request(&set_tx_block_count, unused_response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to set block count for RPMB request: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }

  const sdmmc_buffer_region_t write_region = {
      .buffer = {.vmo = request.tx_frames.vmo.get()},
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = request.tx_frames.offset,
      .size = tx_frame_count * kFrameSize,
  };
  const sdmmc_req_t write_tx_frames = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0,  // Ignored by the card.
      .blocksize = kFrameSize,
      .buffers_list = &write_region,
      .buffers_count = 1,
  };
  if ((status = sdmmc_->Request(&write_tx_frames, unused_response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to write RPMB frames: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }

  if (!read_needed) {
    return ZX_OK;
  }

  const sdmmc_req_t set_rx_block_count = {
      .cmd_idx = SDMMC_SET_BLOCK_COUNT,
      .cmd_flags = SDMMC_SET_BLOCK_COUNT_FLAGS,
      .arg = static_cast<uint32_t>(rx_frame_count),
  };
  if ((status = sdmmc_->Request(&set_rx_block_count, unused_response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to set block count for RPMB request: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }

  const sdmmc_buffer_region_t read_region = {
      .buffer = {.vmo = request.rx_frames.vmo.get()},
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = request.rx_frames.offset,
      .size = rx_frame_count * kFrameSize,
  };
  const sdmmc_req_t read_rx_frames = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0,
      .blocksize = kFrameSize,
      .buffers_list = &read_region,
      .buffers_count = 1,
  };
  if ((status = sdmmc_->Request(&read_rx_frames, unused_response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to read RPMB frames: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }

  return ZX_OK;
}

zx_status_t SdmmcBlockDevice::SetPartition(const EmmcPartition partition) {
  // SetPartition is only called by the worker thread.
  static EmmcPartition current_partition = EmmcPartition::USER_DATA_PARTITION;

  if (is_sd_ || partition == current_partition) {
    return ZX_OK;
  }

  const uint8_t partition_config_value =
      (raw_ext_csd_[MMC_EXT_CSD_PARTITION_CONFIG] & MMC_EXT_CSD_PARTITION_ACCESS_MASK) | partition;

  zx_status_t status = MmcDoSwitch(MMC_EXT_CSD_PARTITION_CONFIG, partition_config_value);
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to switch to partition %u", partition);
    properties_.io_errors_.Add(1);
    return status;
  }

  current_partition = partition;
  return ZX_OK;
}

void SdmmcBlockDevice::Queue(BlockOperation txn) {
  block_op_t* btxn = txn.operation();

  const uint64_t max = txn.private_storage()->block_count;
  switch (btxn->command.opcode) {
    case BLOCK_OPCODE_READ:
    case BLOCK_OPCODE_WRITE:
      if (zx_status_t status = block::CheckIoRange(btxn->rw, max, logger()); status != ZX_OK) {
        BlockComplete(txn, status);
        return;
      }
      // MMC supports FUA writes, but not FUA reads. SD does not support FUA.
      if (btxn->command.flags & BLOCK_IO_FLAG_FORCE_ACCESS) {
        BlockComplete(txn, ZX_ERR_NOT_SUPPORTED);
        return;
      }
      break;
    case BLOCK_OPCODE_TRIM:
      if (zx_status_t status = block::CheckIoRange(btxn->trim, max, logger()); status != ZX_OK) {
        BlockComplete(txn, status);
        return;
      }
      break;
    case BLOCK_OPCODE_FLUSH:
      // queue the flush op. because there is no out of order execution in this
      // driver, when this op gets processed all previous ops are complete.
      break;
    default:
      BlockComplete(txn, ZX_ERR_NOT_SUPPORTED);
      return;
  }

  fbl::AutoLock lock(&lock_);

  txn_list_.push(std::move(txn));
  // Wake up the worker thread.
  worker_event_.Broadcast();
}

void SdmmcBlockDevice::RpmbQueue(RpmbRequestInfo info) {
  using fuchsia_hardware_rpmb::wire::kFrameSize;

  if (info.tx_frames.size % kFrameSize != 0) {
    FDF_LOGL(ERROR, logger(), "tx frame buffer size not a multiple of %u", kFrameSize);
    info.completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  // Checking against SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS is sufficient for casting to uint16_t.
  static_assert(SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS <= UINT16_MAX);

  const uint64_t tx_frame_count = info.tx_frames.size / kFrameSize;
  if (tx_frame_count == 0) {
    info.completer.ReplyError(ZX_OK);
    return;
  }

  if (tx_frame_count > SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS) {
    FDF_LOGL(ERROR, logger(), "received %lu tx frames, maximum is %u", tx_frame_count,
             SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS);
    info.completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
    return;
  }

  if (info.rx_frames.vmo.is_valid()) {
    if (info.rx_frames.size % kFrameSize != 0) {
      FDF_LOGL(ERROR, logger(), "rx frame buffer size is not a multiple of %u", kFrameSize);
      info.completer.ReplyError(ZX_ERR_INVALID_ARGS);
      return;
    }

    const uint64_t rx_frame_count = info.rx_frames.size / kFrameSize;
    if (rx_frame_count > SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS) {
      FDF_LOGL(ERROR, logger(), "received %lu rx frames, maximum is %u", rx_frame_count,
               SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS);
      info.completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
      return;
    }
  }

  fbl::AutoLock lock(&lock_);
  if (rpmb_list_.size() >= kMaxOutstandingRpmbRequests) {
    info.completer.ReplyError(ZX_ERR_SHOULD_WAIT);
  } else {
    rpmb_list_.push_back(std::move(info));
    worker_event_.Broadcast();
  }
}

void SdmmcBlockDevice::HandleBlockOps(block::BorrowedOperationQueue<PartitionInfo>& txn_list) {
  for (size_t i = 0; i < kRoundRobinRequestCount; i++) {
    std::optional<BlockOperation> txn = txn_list.pop();
    if (!txn) {
      break;
    }

    std::vector<BlockOperation> btxns;
    btxns.push_back(*std::move(txn));

    const block_op_t& bop = *btxns[0].operation();
    const uint8_t op = bop.command.opcode;
    const EmmcPartition partition = btxns[0].private_storage()->partition;

    zx_status_t status = ZX_ERR_INVALID_ARGS;
    if (op == BLOCK_OPCODE_READ || op == BLOCK_OPCODE_WRITE) {
      const char* const trace_name = op == BLOCK_OPCODE_READ ? "read" : "write";
      TRACE_DURATION_BEGIN("sdmmc", trace_name);

      // Consider trailing txns for eMMC Command Packing (batching)
      if (partition == USER_DATA_PARTITION) {
        const uint32_t max_command_packing =
            (op == BLOCK_OPCODE_READ) ? max_packed_reads_effective_ : max_packed_writes_effective_;
        // The system page size is used below, because the header block requires its own
        // scatter-gather transfer descriptor in the lower-level SDMMC driver.
        uint64_t cum_transfer_bytes = (bop.rw.length * block_info_.block_size) +
                                      zx_system_get_page_size();  // +1 page for header block.
        while (btxns.size() < max_command_packing) {
          // TODO(https://fxbug.dev/42083080): It's inefficient to pop() here only to push() later
          // in the case of packing ineligibility. Later on, we'll likely move away from using
          // block::BorrowedOperationQueue once we start using the FIDL driver transport arena (at
          // which point, use something like peek() instead).
          std::optional<BlockOperation> pack_candidate_txn = txn_list.pop();
          if (!pack_candidate_txn) {
            // No more candidate txns to consider for packing.
            break;
          }

          cum_transfer_bytes += pack_candidate_txn->operation()->rw.length * block_info_.block_size;
          // TODO(https://fxbug.dev/42083080): Explore reordering commands for more command packing.
          if (pack_candidate_txn->operation()->command.opcode != bop.command.opcode ||
              pack_candidate_txn->private_storage()->partition != partition ||
              cum_transfer_bytes > block_info_.max_transfer_size) {
            // Candidate txn is ineligible for packing.
            txn_list.push(std::move(*pack_candidate_txn));
            break;
          }

          btxns.push_back(std::move(*pack_candidate_txn));
        }
      }

      status = ReadWriteWithRetries(btxns, partition);

      TRACE_DURATION_END("sdmmc", trace_name, "opcode", TA_INT32(bop.rw.command.opcode), "extra",
                         TA_INT32(bop.rw.extra), "length", TA_INT32(bop.rw.length), "offset_vmo",
                         TA_INT64(bop.rw.offset_vmo), "offset_dev", TA_INT64(bop.rw.offset_dev),
                         "txn_status", TA_INT32(status));
    } else if (op == BLOCK_OPCODE_TRIM) {
      TRACE_DURATION_BEGIN("sdmmc", "trim");

      status = Trim(bop.trim, partition);

      TRACE_DURATION_END("sdmmc", "trim", "opcode", TA_INT32(bop.trim.command.opcode), "length",
                         TA_INT32(bop.trim.length), "offset_dev", TA_INT64(bop.trim.offset_dev),
                         "txn_status", TA_INT32(status));
    } else if (op == BLOCK_OPCODE_FLUSH) {
      TRACE_DURATION_BEGIN("sdmmc", "flush");

      status = Flush();

      TRACE_DURATION_END("sdmmc", "flush", "opcode", TA_INT32(bop.command.opcode), "txn_status",
                         TA_INT32(status));
    } else {
      // should not get here
      FDF_LOGL(ERROR, logger(), "invalid block op %d", op);
      TRACE_INSTANT("sdmmc", "unknown", TRACE_SCOPE_PROCESS, "opcode",
                    TA_INT32(bop.rw.command.opcode), "txn_status", TA_INT32(status));
      __UNREACHABLE;
    }

    for (auto& btxn : btxns) {
      BlockComplete(btxn, status);
    }
  }
}

void SdmmcBlockDevice::HandleRpmbRequests(std::deque<RpmbRequestInfo>& rpmb_list) {
  for (size_t i = 0; i < kRoundRobinRequestCount && !rpmb_list.empty(); i++) {
    RpmbRequestInfo& request = *rpmb_list.begin();
    zx_status_t status = RpmbRequest(request);
    if (status == ZX_OK) {
      request.completer.ReplySuccess();
    } else {
      request.completer.ReplyError(status);
    }

    rpmb_list.pop_front();
  }
}

void SdmmcBlockDevice::WorkerLoop() {
  for (;;) {
    TRACE_DURATION("sdmmc", "work loop");

    block::BorrowedOperationQueue<PartitionInfo> txn_list;
    std::deque<RpmbRequestInfo> rpmb_list;

    {
      fbl::AutoLock lock(&lock_);
      while (txn_list_.is_empty() && rpmb_list_.empty() && !shutdown_) {
        worker_event_.Wait(&lock_);
      }

      if (shutdown_) {
        break;
      }

      txn_list = std::move(txn_list_);
      rpmb_list.swap(rpmb_list_);
    }

    while (!txn_list.is_empty() || !rpmb_list.empty()) {
      HandleBlockOps(txn_list);
      HandleRpmbRequests(rpmb_list);
    }
  }

  FDF_LOGL(DEBUG, logger(), "worker thread terminated successfully");
}

zx_status_t SdmmcBlockDevice::SuspendPower() {
  fbl::AutoLock lock(&power_lock_);

  if (power_suspended_ == true) {
    return ZX_OK;
  }

  // TODO(b/309152727): Finish serving requests currently in the queue, if any.

  if (zx_status_t status = Flush(); status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to flush: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = sdmmc_->MmcSelectCard(/*select=*/false); status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to (de-)SelectCard before sleep: %s",
             zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = sdmmc_->MmcSleepOrAwake(/*sleep=*/true); status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to sleep: %s", zx_status_get_string(status));
    return status;
  }

  power_suspended_ = true;
  properties_.power_suspended_.Set(power_suspended_);
  return ZX_OK;
}

zx_status_t SdmmcBlockDevice::ResumePower() {
  fbl::AutoLock lock(&power_lock_);

  if (power_suspended_ == false) {
    return ZX_OK;
  }

  if (zx_status_t status = sdmmc_->MmcSleepOrAwake(/*sleep=*/false); status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to awake: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = sdmmc_->MmcSelectCard(/*select=*/false); status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to SelectCard after awake: %s", zx_status_get_string(status));
    return status;
  }

  power_suspended_ = false;
  properties_.power_suspended_.Set(power_suspended_);
  return ZX_OK;
}

zx_status_t SdmmcBlockDevice::WaitForTran() {
  uint32_t current_state;
  size_t attempt = 0;
  for (; attempt <= kTranMaxAttempts; attempt++) {
    uint32_t response;
    zx_status_t st = sdmmc_->SdmmcSendStatus(&response);
    if (st != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "SDMMC_SEND_STATUS error, retcode = %d", st);
      return st;
    }

    current_state = MMC_STATUS_CURRENT_STATE(response);
    if (current_state == MMC_STATUS_CURRENT_STATE_RECV) {
      st = sdmmc_->SdmmcStopTransmission();
      continue;
    } else if (current_state == MMC_STATUS_CURRENT_STATE_TRAN) {
      break;
    }

    zx::nanosleep(zx::deadline_after(zx::msec(10)));
  }

  if (attempt == kTranMaxAttempts) {
    // Too many retries, fail.
    return ZX_ERR_TIMED_OUT;
  } else {
    return ZX_OK;
  }
}

void SdmmcBlockDevice::SetBlockInfo(uint32_t block_size, uint64_t block_count) {
  block_info_.block_size = block_size;
  block_info_.block_count = block_count;
}

fdf::Logger& SdmmcBlockDevice::logger() { return parent_->logger(); }

}  // namespace sdmmc
