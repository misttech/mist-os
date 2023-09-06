// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "logical_unit.h"

#include "fbl/alloc_checker.h"
#include "src/devices/block/lib/common/include/common.h"

namespace ufs {

zx_status_t LogicalUnit::AddLogicalUnit() { return DdkAdd(ddk::DeviceAddArgs(LunName().c_str())); }

zx_status_t LogicalUnit::Bind(Ufs &controller, BlockDevice &block_device, const uint8_t lun_id) {
  fbl::AllocChecker ac;
  auto lun_driver = fbl::make_unique_checked<LogicalUnit>(&ac, controller.zxdev(), lun_id,
                                                          &block_device, controller);
  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate memory for logical unit %u.", lun_id);
    return ZX_ERR_NO_MEMORY;
  }

  if (zx_status_t status = lun_driver->AddLogicalUnit(); status != ZX_OK) {
    zxlogf(ERROR, "Failed to make LogicalUnit:%d", lun_id);
    return status;
  }

  // The DDK takes ownership of the device.
  [[maybe_unused]] auto placeholder = lun_driver.release();
  return ZX_OK;
}

void LogicalUnit::DdkRelease() {
  zxlogf(DEBUG, "Releasing driver.");
  delete this;
}

void LogicalUnit::DdkInit(ddk::InitTxn txn) {
  // The driver initialization has numerous error conditions. Wrap the initialization here to ensure
  // we always call txn.Reply() in any outcome.
  zx_status_t status = Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Driver initialization failed: %s", zx_status_get_string(status));
  }
  txn.Reply(status);
}

zx_status_t LogicalUnit::Init() {
  // UFS r/w commands operate in block units, maximum of 65535 blocks.
  const uint32_t max_bytes_per_cmd = block_info_.block_size * 65535;

  // The maximum transfer size supported by UFSHCI spec is 65535 * 256 KiB. However, we limit the
  // maximum transfer size to 1MiB for performance reason.
  const uint32_t max_transfer_bytes = std::min(kMaxTransferSize1MiB, max_bytes_per_cmd);
  block_info_.max_transfer_size = max_transfer_bytes;
  ZX_DEBUG_ASSERT(max_transfer_bytes % block_info_.block_size == 0);
  max_transfer_blocks_ = max_transfer_bytes / block_info_.block_size;
  return ZX_OK;
}

void LogicalUnit::BlockImplQuery(block_info_t *info_out, size_t *block_op_size_out) {
  *info_out = block_info_;
  *block_op_size_out = sizeof(IoCommand);
}

void LogicalUnit::BlockImplQueue(block_op_t *op, block_impl_queue_callback callback, void *cookie) {
  IoCommand *io_cmd = containerof(op, IoCommand, op);
  io_cmd->completion_cb = callback;
  io_cmd->cookie = cookie;
  io_cmd->lun_id = lun_id_;
  io_cmd->block_size_bytes = block_info_.block_size;

  const uint32_t opcode = io_cmd->op.command.opcode;
  switch (opcode) {
    case BLOCK_OPCODE_READ:
    case BLOCK_OPCODE_WRITE:
      if (zx_status_t status =
              block::CheckIoRange(op->rw, block_info_.block_count, max_transfer_blocks_);
          status != ZX_OK) {
        io_cmd->Complete(status);
        return;
      }
      zxlogf(TRACE, "Block IO: %s: %u blocks @ LBA %zu", opcode == BLOCK_OPCODE_WRITE ? "wr" : "rd",
             op->rw.length, op->rw.offset_dev);
      break;
    case BLOCK_OPCODE_TRIM:
      // TODO(fxbug.dev/124835): Support TRIM command
      zxlogf(TRACE, "The trim command is not supported.");
      io_cmd->Complete(ZX_ERR_NOT_SUPPORTED);
      return;
    case BLOCK_OPCODE_FLUSH:
      zxlogf(TRACE, "Block IO: flush");
      break;
    default:
      io_cmd->Complete(ZX_ERR_NOT_SUPPORTED);
      return;
  }

  controller_.QueueIoCommand(io_cmd);
}

}  // namespace ufs
