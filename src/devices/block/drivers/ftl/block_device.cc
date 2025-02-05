// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "block_device.h"

#include <lib/ddk/debug.h>
#include <lib/ddk/trace/event.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/zbi-format/partition.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/fidl.h>
#include <zircon/status.h>
#include <zircon/threads.h>
#include <zircon/types.h>

#include <iostream>
#include <mutex>

#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <fbl/algorithm.h>

#include "fidl/fuchsia.storage.ftl/cpp/wire_types.h"
#include "lib/driver/outgoing/cpp/outgoing_directory.h"
#include "lib/fidl/cpp/wire/internal/transport.h"
#include "lib/inspect/cpp/vmo/types.h"
#include "lib/zbi-format/zbi.h"
#include "nand_driver.h"
#include "src/devices/bin/driver_runtime/dispatcher.h"
#include "src/devices/block/drivers/ftl/metrics.h"
#include "src/devices/block/lib/common/include/common-dfv1.h"
#include "zircon/system/public/zircon/errors.h"

namespace {

constexpr char kDeviceName[] = "ftl";

// Encapsulates a block operation that is created by this device (so that it
// goes through the worker thread).
class LocalOperation {
 public:
  explicit LocalOperation(uint8_t opcode) {
    operation_.op.command = {.opcode = opcode, .flags = 0};
  }

  block_op_t* op() { return &operation_.op; }

  // Waits for the completion of the operation. Returns the operation status.
  zx_status_t Execute(ftl::BlockDevice* parent) {
    parent->BlockImplQueue(&operation_.op, OnCompletion, this);
    zx_status_t status = sync_completion_wait(&event_, ZX_SEC(60));
    sync_completion_reset(&event_);
    if (status != ZX_OK) {
      return status;
    }
    return status_;
  }

 private:
  static void OnCompletion(void* cookie, zx_status_t status, block_op_t* op) {
    LocalOperation* operation = reinterpret_cast<LocalOperation*>(cookie);
    ZX_DEBUG_ASSERT(operation);
    operation->status_ = status;
    sync_completion_signal(&operation->event_);
  }

  sync_completion_t event_;
  zx_status_t status_ = ZX_ERR_BAD_STATE;
  ftl::FtlOp operation_ = {};
};

}  // namespace

namespace ftl {

BlockDevice::BlockDevice(zx_device_t* parent, fdf_dispatcher_t* dispatcher)
    : DeviceType(parent), dispatcher_(dispatcher) {}

BlockDevice::~BlockDevice() {
  if (thread_created_) {
    Kill();
    sync_completion_signal(&wake_signal_);
    int result_code;
    thrd_join(worker_, &result_code);
  }
  ZX_ASSERT(list_is_empty(&txn_list_));
  bool volume_created = (params_.GetSize() != 0);
  if (volume_created) {
    if (zx_status_t status = volume_->Unmount(); status != ZX_OK) {
      zxlogf(ERROR, "FTL: FtlUmount() failed: %s", zx_status_get_string(status));
    }
  }
}

zx_status_t BlockDevice::Bind() {
  zxlogf(INFO, "FTL: Binding to parent");

  if (device_get_protocol(parent(), ZX_PROTOCOL_NAND, &parent_) != ZX_OK) {
    zxlogf(ERROR, "FTL: Parent device does not support nand protocol");
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Get the optional bad block protocol.
  if (device_get_protocol(parent(), ZX_PROTOCOL_BAD_BLOCK, &bad_block_) != ZX_OK) {
    zxlogf(WARNING, "FTL: Parent device does not support bad_block protocol");
  }

  zx_status_t status = Init();
  if (status != ZX_OK) {
    return status;
  }

  ddk::DeviceAddArgs args(kDeviceName);
  args.set_inspect_vmo(metrics_.DuplicateInspectVmo());

  std::array service{fuchsia_storage_ftl::Service::Name};
  if (dispatcher_ != nullptr) {
    outgoing_.emplace(dispatcher_);
    zx::result result = outgoing_->AddService<fuchsia_storage_ftl::Service>(
        fuchsia_storage_ftl::Service::InstanceHandler({
            .config = config_binding_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure),
        }));

    auto [client, server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
    result = outgoing_->Serve(std::move(server));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to service the outgoing directory");
      return result.status_value();
    }
    args.set_fidl_service_offers(service).set_outgoing_dir(client.TakeChannel());
  }
  return DdkAdd(args);
}

void BlockDevice::DdkUnbind(ddk::UnbindTxn txn) {
  Kill();
  sync_completion_signal(&wake_signal_);
  txn.Reply();
}

zx_status_t BlockDevice::Init() {
  ZX_DEBUG_ASSERT(!thread_created_);
  if (thrd_create_with_name(&worker_, WorkerThreadStub, this, "ftl_worker") != thrd_success) {
    return ZX_ERR_NO_RESOURCES;
  }
  thread_created_ = true;

  // Set a scheduling role for the ftl_worker thread.
  // This is required in order to service the blobfs-pager-thread, which is on a deadline profile.
  // This will no longer be needed once we have the ability to propagate deadlines. Until then, we
  // need to set deadline profiles for all threads that the blobfs-pager-thread interacts with in
  // order to service page requests.
  const char* role_name = "fuchsia.devices.block.drivers.ftl.device";
  const zx_status_t status = device_set_profile_by_role(parent(), thrd_get_zx_handle(worker_),
                                                        role_name, strlen(role_name));
  if (status != ZX_OK) {
    zxlogf(WARNING, "FTL: Failed to apply role to worker: %d\n", status);
  }

  if (!InitFtl()) {
    return ZX_ERR_NO_RESOURCES;
  }

  return ZX_OK;
}

zx_status_t BlockDevice::Suspend() {
  LocalOperation operation(BLOCK_OPCODE_FLUSH);
  return operation.Execute(this);
}

void BlockDevice::DdkSuspend(ddk::SuspendTxn txn) {
  zxlogf(INFO, "FTL: Suspend");
  zx_status_t status = Suspend();
  txn.Reply(status, txn.requested_state());
}

zx_status_t BlockDevice::DdkGetProtocol(uint32_t proto_id, void* out_protocol) {
  auto* proto = static_cast<ddk::AnyProtocol*>(out_protocol);
  proto->ctx = this;
  switch (proto_id) {
    case ZX_PROTOCOL_BLOCK_IMPL:
      proto->ops = &block_impl_protocol_ops_;
      return ZX_OK;
    case ZX_PROTOCOL_BLOCK_PARTITION:
      proto->ops = &block_partition_protocol_ops_;
      return ZX_OK;
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

void BlockDevice::BlockImplQuery(block_info_t* info_out, size_t* block_op_size_out) {
  zxlogf(DEBUG, "FTL: Query");
  memset(info_out, 0, sizeof(*info_out));
  info_out->block_count = params_.num_pages;
  info_out->block_size = params_.page_size;
  info_out->flags = FLAG_TRIM_SUPPORT;
  info_out->max_transfer_size = fuchsia_hardware_block::wire::kMaxTransferUnbounded;
  *block_op_size_out = sizeof(FtlOp);
}

void BlockDevice::BlockImplQueue(block_op_t* operation, block_impl_queue_callback completion_cb,
                                 void* cookie) {
  zxlogf(DEBUG, "FTL: Queue");
  switch (operation->command.opcode) {
    case BLOCK_OPCODE_WRITE:
    case BLOCK_OPCODE_READ: {
      if (zx_status_t status = block::CheckIoRange(operation->rw, params_.num_pages);
          status != ZX_OK) {
        completion_cb(cookie, status, operation);
        return;
      }
      if (operation->command.flags & BLOCK_IO_FLAG_FORCE_ACCESS) {
        completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, operation);
        return;
      }
      break;
    }
    case BLOCK_OPCODE_TRIM:
      if (zx_status_t status = block::CheckIoRange(operation->trim, params_.num_pages);
          status != ZX_OK) {
        completion_cb(cookie, status, operation);
        return;
      }
      break;

    case BLOCK_OPCODE_FLUSH:
      break;

    default:
      completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, operation);
      return;
  }

  FtlOp* block_op = reinterpret_cast<FtlOp*>(operation);
  block_op->completion_cb = completion_cb;
  block_op->cookie = cookie;
  if (AddToList(block_op)) {
    sync_completion_signal(&wake_signal_);
  } else {
    completion_cb(cookie, ZX_ERR_BAD_STATE, operation);
  }
}

zx_status_t BlockDevice::BlockPartitionGetGuid(guidtype_t guid_type, guid_t* out_guid) {
  if (guid_type != GUIDTYPE_TYPE) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  memcpy(out_guid, guid_, ZBI_PARTITION_GUID_LEN);
  return ZX_OK;
}

zx_status_t BlockDevice::BlockPartitionGetName(char* out_name, size_t capacity) {
  if (capacity < sizeof(kDeviceName)) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }
  strncpy(out_name, kDeviceName, capacity);
  return ZX_OK;
}

zx_status_t BlockDevice::BlockPartitionGetMetadata(partition_metadata_t* out_metadata) {
  strlcpy(out_metadata->name, kDeviceName, sizeof(out_metadata->name));
  memcpy(&out_metadata->type_guid, guid_, ZBI_PARTITION_GUID_LEN);
  memset(&out_metadata->instance_guid, 0, ZBI_PARTITION_GUID_LEN);
  out_metadata->start_block_offset = 0;
  out_metadata->num_blocks = params_.num_pages;
  out_metadata->flags = 0;
  return ZX_OK;
}

bool BlockDevice::OnVolumeAdded(uint32_t page_size, uint32_t num_pages) {
  params_ = {page_size, num_pages};
  zxlogf(INFO, "FTL: %d pages of %d bytes", num_pages, page_size);
  return true;
}

zx_status_t BlockDevice::FormatInternal() {
  std::lock_guard<std::mutex> lock(volume_lock_);
  zx_status_t status = volume_->Format();
  if (status != ZX_OK) {
    zxlogf(ERROR, "FTL: format failed: %s", zx_status_get_string(status));
  }
  return status;
}

bool BlockDevice::InitFtl() {
  std::unique_ptr<NandDriver> driver =
      NandDriver::CreateWithCounters(&parent_, &bad_block_, &nand_counters_, 0);
  const char* error = driver->Init();
  if (error) {
    zxlogf(ERROR, "Failed to init FTL driver: %s", error);
    return false;
  }
  memcpy(guid_, driver->info().partition_guid, ZBI_PARTITION_GUID_LEN);

  std::lock_guard<std::mutex> lock(volume_lock_);
  if (!volume_) {
    volume_ = std::make_unique<ftl::VolumeImpl>(this);
  }

  error = volume_->Init(std::move(driver));
  if (error) {
    zxlogf(ERROR, "Failed to init FTL volume: %s", error);
    return false;
  }

  Volume::Stats stats;
  if (volume_->GetStats(&stats) == ZX_OK) {
    zxlogf(INFO, "FTL: Wear count: %u, Garbage level: %d%%", stats.wear_count, stats.garbage_level);
    metrics_.max_wear().Set(stats.wear_count);
    metrics_.initial_bad_blocks().Set(stats.initial_bad_blocks);
    metrics_.running_bad_blocks().Set(stats.running_bad_blocks);

    static_assert(std::size(stats.map_block_end_page_failure_reasons) == Metrics::kReasonCount);
    for (int i = 0; i < Metrics::kReasonCount; ++i) {
      metrics_.map_block_end_page_failure_reason(i).Set(
          stats.map_block_end_page_failure_reasons[i]);
    }
  }

  zxlogf(INFO, "FTL: InitFtl ok");
  return true;
}

void BlockDevice::Kill() {
  std::lock_guard<std::mutex> lock(lock_);
  dead_ = true;
}

bool BlockDevice::AddToList(FtlOp* operation) {
  std::lock_guard<std::mutex> lock(lock_);
  if (!dead_) {
    list_add_tail(&txn_list_, &operation->node);
  }
  return !dead_;
}

bool BlockDevice::RemoveFromList(FtlOp** operation) {
  std::lock_guard<std::mutex> lock(lock_);
  *operation = list_remove_head_type(&txn_list_, FtlOp, node);
  return !dead_;
}

int BlockDevice::WorkerThread() {
  for (;;) {
    FtlOp* operation;
    for (;;) {
      bool alive = RemoveFromList(&operation);
      if (operation) {
        if (alive) {
          sync_completion_reset(&wake_signal_);
          break;
        } else {
          operation->completion_cb(operation->cookie, ZX_ERR_BAD_STATE, &operation->op);
        }
      } else if (alive) {
        // Flush any pending data after 15 seconds of inactivity. This is
        // meant to reduce the chances of data loss if power is removed.
        // This value is only a guess.
        zx_duration_t timeout = pending_flush_ ? ZX_SEC(15) : ZX_TIME_INFINITE;
        zx_status_t status = sync_completion_wait(&wake_signal_, timeout);
        if (status == ZX_ERR_TIMED_OUT) {
          std::lock_guard<std::mutex> lock(volume_lock_);
          Flush();
          pending_flush_ = false;
        }
      } else {
        return 0;
      }
    }

    zx_status_t status = ZX_OK;

    // These counters are updated by the NdmDriver implementation, which will keep track of the
    // number of operation issued, during the context of this block operation, to the nand driver.
    //
    // The counters are reset before each block operation, so the numbers reflect the number of nand
    // operations issued as a result of this operation alone.
    //
    // These operations are then aggregated into the respective |BlockOperationProperties| of the
    // given block operation type.
    nand_counters_.Reset();
    ftl::BlockOperationProperties* op_stats = nullptr;
    zx_status_t metrics_status;
    Volume::Counters counters;
    {
      std::lock_guard<std::mutex> lock(volume_lock_);
      TRACE_DURATION_BEGIN("block:ftl", "Operation", "opcode", operation->op.command.opcode,
                           "offset_dev", operation->op.rw.offset_dev, "length",
                           operation->op.rw.length);
      switch (operation->op.command.opcode) {
        case BLOCK_OPCODE_WRITE:
          pending_flush_ = true;
          status = ReadWriteData(&operation->op);
          op_stats = &metrics_.write();
          break;

        case BLOCK_OPCODE_READ:
          pending_flush_ = true;
          status = ReadWriteData(&operation->op);
          op_stats = &metrics_.read();
          break;

        case BLOCK_OPCODE_TRIM:
          pending_flush_ = true;
          status = TrimData(&operation->op);
          op_stats = &metrics_.trim();
          break;

        case BLOCK_OPCODE_FLUSH: {
          status = Flush();
          pending_flush_ = false;
          op_stats = &metrics_.flush();
          break;
        }
        default:
          ZX_DEBUG_ASSERT(false);  // Unexpected.
      }
      TRACE_DURATION_END("block:ftl", "Operation", "nand_ops", nand_counters_.GetSum());
      metrics_status = volume_->GetCounters(&counters);
    }

    if (metrics_status == ZX_OK) {
      metrics_.max_wear().Set(counters.wear_count);
      metrics_.initial_bad_blocks().Set(counters.initial_bad_blocks);
      metrics_.running_bad_blocks().Set(counters.running_bad_blocks);
    }

    // Update all counters and rates for the supported operation type.
    if (op_stats != nullptr) {
      op_stats->count.Add(1);
      op_stats->all.count.Add(nand_counters_.GetSum());
      op_stats->all.rate.Add(nand_counters_.GetSum());
      op_stats->block_erase.count.Add(nand_counters_.block_erase);
      op_stats->block_erase.rate.Add(nand_counters_.block_erase);
      op_stats->page_write.count.Add(nand_counters_.page_write);
      op_stats->page_write.rate.Add(nand_counters_.page_write);
      op_stats->page_read.count.Add(nand_counters_.page_read);
      op_stats->page_read.rate.Add(nand_counters_.page_read);
    }
    operation->completion_cb(operation->cookie, status, &operation->op);
  }
}

int BlockDevice::WorkerThreadStub(void* arg) {
  BlockDevice* device = reinterpret_cast<BlockDevice*>(arg);
  return device->WorkerThread();
}

zx_status_t BlockDevice::ReadWriteData(block_op_t* operation) {
  uint64_t addr = operation->rw.offset_vmo * params_.page_size;
  uint32_t length = operation->rw.length * params_.page_size;
  uint32_t offset = static_cast<uint32_t>(operation->rw.offset_dev);
  if (offset != operation->rw.offset_dev) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // TODO(https://fxbug.dev/42107473): We may go back to ask the kernel to copy the data for us
  // if that ends up being more efficient.
  fzl::VmoMapper mapper;
  zx_status_t status = mapper.Map(*zx::unowned_vmo(operation->rw.vmo), addr, length,
                                  ZX_VM_FLAG_PERM_READ | ZX_VM_FLAG_PERM_WRITE | ZX_VM_MAP_RANGE);
  if (status != ZX_OK) {
    return status;
  }

  if (operation->command.opcode == BLOCK_OPCODE_WRITE) {
    zxlogf(TRACE, "FTL: BLK To write %d blocks at %d :", operation->rw.length, offset);
    status = volume_->Write(offset, operation->rw.length, mapper.start());
    if (status != ZX_OK) {
      zxlogf(ERROR, "FTL: Failed to write %u@%u: %s", operation->rw.length, offset,
             zx_status_get_string(status));
      return status;
    }
  }

  if (operation->command.opcode == BLOCK_OPCODE_READ) {
    zxlogf(TRACE, "FTL: BLK To read %d blocks at %d :", operation->rw.length, offset);
    status = volume_->Read(offset, operation->rw.length, mapper.start());
    if (status != ZX_OK) {
      zxlogf(ERROR, "FTL: Failed to read %u@%u: %s", operation->rw.length, offset,
             zx_status_get_string(status));
      return status;
    }
  }

  return ZX_OK;
}

zx_status_t BlockDevice::TrimData(block_op_t* operation) {
  uint32_t offset = static_cast<uint32_t>(operation->trim.offset_dev);
  if (offset != operation->trim.offset_dev) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  ZX_DEBUG_ASSERT(operation->command.opcode == BLOCK_OPCODE_TRIM);
  zxlogf(TRACE, "FTL: BLK To trim %d blocks at %d :", operation->trim.length, offset);
  zx_status_t status = volume_->Trim(offset, operation->trim.length);
  if (status != ZX_OK) {
    zxlogf(ERROR, "FTL: Failed to trim: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t BlockDevice::Flush() {
  zx_status_t status = volume_->Flush();
  if (status != ZX_OK) {
    zxlogf(ERROR, "FTL: flush failed: %s", zx_status_get_string(status));
    return status;
  }

  zxlogf(TRACE, "FTL: Finished flush");
  return status;
}

void BlockDevice::SetVolumeForTest(std::unique_ptr<ftl::Volume> volume) {
  std::lock_guard<std::mutex> lock(volume_lock_);
  volume_ = std::move(volume);
}

void BlockDevice::Get(GetCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(volume_lock_);
  if (!volume_) {
    return completer.ReplyError(ZX_ERR_UNAVAILABLE);
  }
  bool state;
  if (zx_status_t result = volume_->GetNewWearLeveling(&state); result != ZX_OK) {
    return completer.ReplyError(result);
  }
  fidl::WireTableFrame<fuchsia_storage_ftl::wire::ConfigurationOptions> options_frame;
  return completer.ReplySuccess(
      fuchsia_storage_ftl::wire::ConfigurationOptions::ExternalBuilder(
          fidl::ObjectView<fidl::WireTableFrame<fuchsia_storage_ftl::wire::ConfigurationOptions>>::
              FromExternal(&options_frame))
          .use_new_wear_leveling(state)
          .Build());
}

void BlockDevice::Set(fuchsia_storage_ftl::wire::ConfigurationOptions* request,
                      SetCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(volume_lock_);
  if (!volume_) {
    return completer.ReplyError(ZX_ERR_UNAVAILABLE);
  }

  if (!request->has_use_new_wear_leveling()) {
    return completer.ReplyError(ZX_ERR_INVALID_ARGS);
  }

  if (zx_status_t result = volume_->SetNewWearLeveling(request->use_new_wear_leveling());
      result != ZX_OK) {
    return completer.ReplyError(result);
  }
  return completer.ReplySuccess();
}

}  // namespace ftl.
