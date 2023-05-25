// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "logical_unit.h"

#include "fbl/alloc_checker.h"
#include "src/devices/block/lib/common/include/common.h"

namespace ufs {

zx_status_t LogicalUnit::AddDevice() { return DdkAdd(ddk::DeviceAddArgs(LunName().c_str())); }

zx_status_t LogicalUnit::Bind(Ufs &controller, BlockDevice &block_device, const uint8_t lun_id) {
  fbl::AllocChecker ac;
  auto lun_driver = fbl::make_unique_checked<LogicalUnit>(&ac, controller.zxdev(), lun_id,
                                                          &block_device, controller);
  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate memory for logical unit %u.", lun_id);
    return ZX_ERR_NO_MEMORY;
  }

  if (zx_status_t status = lun_driver->AddDevice(); status != ZX_OK) {
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
  io_cmd->block_count = block_info_.block_count;

  const uint32_t opcode = io_cmd->op.command & BLOCK_OP_MASK;
  switch (opcode) {
    case BLOCK_OP_READ:
    case BLOCK_OP_WRITE:
      if (zx_status_t status = block::CheckIoRange(op->rw, block_info_.block_count);
          status != ZX_OK) {
        io_cmd->Complete(status);
      }
      zxlogf(TRACE, "Block IO: %s: %u blocks @ LBA %zu", opcode == BLOCK_OP_WRITE ? "wr" : "rd",
             op->rw.length, op->rw.offset_dev);
      break;
    case BLOCK_OP_TRIM:
      zxlogf(TRACE, "Block IO: trim");
      // TODO(fxbug.dev/124835): Support TRIM command
      io_cmd->Complete(ZX_ERR_NOT_SUPPORTED);
      break;
    case BLOCK_OP_FLUSH:
      zxlogf(TRACE, "Block IO: flush");
      break;
    default:
      io_cmd->Complete(ZX_ERR_NOT_SUPPORTED);
      return;
  }

  controller_.HandleBlockOp(io_cmd);
}

}  // namespace ufs
