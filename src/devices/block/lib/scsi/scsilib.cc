// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <endian.h>
#include <fuchsia/hardware/block/c/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/scsi/scsilib.h>
#include <netinet/in.h>
#include <zircon/process.h>

#include <fbl/alloc_checker.h>

namespace scsi {

zx::result<fbl::RefPtr<Disk>> Disk::Bind(Controller* controller, zx_device_t* parent,
                                         uint8_t target, uint16_t lun,
                                         uint32_t max_transfer_size_blocks) {
  fbl::AllocChecker ac;
  auto disk = fbl::MakeRefCountedChecked<Disk>(&ac, controller, parent, target, lun,
                                               max_transfer_size_blocks);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  auto status = disk->Add();
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(disk);
}

zx_status_t Disk::Add() {
  zx::result inquiry_data = controller_->Inquiry(target_, lun_);
  if (inquiry_data.is_error()) {
    return inquiry_data.status_value();
  }
  // Check that its a disk first.
  if (inquiry_data.value().peripheral_device_type != 0) {
    return ZX_ERR_IO;
  }

  // Print T10 Vendor ID/Product ID
  zxlogf(INFO, "%d:%d ", target_, lun_);
  for (int i = 0; i < 8; i++) {
    zxlogf(INFO, "%c", inquiry_data.value().t10_vendor_id[i]);
  }
  zxlogf(INFO, " ");
  for (int i = 0; i < 16; i++) {
    zxlogf(INFO, "%c", inquiry_data.value().product_id[i]);
  }
  zxlogf(INFO, "");

  removable_ = inquiry_data.value().removable_media();

  zx::result mode_sense_data = controller_->ModeSense(target_, lun_);
  if (mode_sense_data.is_error()) {
    return mode_sense_data.status_value();
  }
  write_protected_ = mode_sense_data.value().write_protected();

  zx::result write_cache_enabled = controller_->ModeSenseWriteCacheEnabled(target_, lun_);
  if (write_cache_enabled.is_error()) {
    return write_cache_enabled.status_value();
  }
  write_cache_enabled_ = write_cache_enabled.value();

  zx_status_t status = controller_->ReadCapacity(target_, lun_, &block_count_, &block_size_bytes_);
  if (status != ZX_OK) {
    return status;
  }

  zxlogf(INFO, "%ld blocks of %d bytes", block_count_, block_size_bytes_);

  status = DdkAdd(DiskName().c_str());
  if (status == ZX_OK) {
    AddRef();
  }
  return status;
}

void Disk::DdkRelease() {
  if (Release()) {
    delete this;
  }
}

void Disk::BlockImplQuery(block_info_t* info_out, size_t* block_op_size_out) {
  info_out->block_size = block_size_bytes_;
  info_out->block_count = block_count_;
  info_out->max_transfer_size = block_size_bytes_ * max_transfer_size_blocks_;
  info_out->flags =
      (write_protected_ ? BLOCK_FLAG_READONLY : 0) | (removable_ ? BLOCK_FLAG_REMOVABLE : 0);
  *block_op_size_out = controller_->BlockOpSize();
}

void Disk::BlockImplQueue(block_op_t* op, block_impl_queue_callback completion_cb, void* cookie) {
  DiskOp* disk_op = containerof(op, DiskOp, op);
  disk_op->completion_cb = completion_cb;
  disk_op->cookie = cookie;

  auto op_type = op->command & BLOCK_OP_MASK;
  switch (op_type) {
    case BLOCK_OP_READ: {
      Read16CDB cdb = {};
      cdb.opcode = Opcode::READ_16;
      cdb.logical_block_address = htobe64(op->rw.offset_dev);
      cdb.transfer_length = htonl(op->rw.length);
      controller_->ExecuteCommandAsync(target_, lun_, {&cdb, sizeof(cdb)},
                                       /*is_write=*/false, block_size_bytes_, disk_op);
      break;
    }
    case BLOCK_OP_WRITE: {
      Write16CDB cdb = {};
      cdb.opcode = Opcode::WRITE_16;
      if (op->command & BLOCK_FL_FORCE_ACCESS) {
        cdb.set_force_unit_access(true);
      }
      cdb.logical_block_address = htobe64(op->rw.offset_dev);
      cdb.transfer_length = htonl(op->rw.length);
      controller_->ExecuteCommandAsync(target_, lun_, {&cdb, sizeof(cdb)},
                                       /*is_write=*/true, block_size_bytes_, disk_op);
      break;
    }
    case BLOCK_OP_FLUSH: {
      if (!write_cache_enabled_) {
        completion_cb(cookie, ZX_OK, op);
        return;
      }
      SynchronizeCache10CDB cdb = {};
      cdb.opcode = Opcode::SYNCHRONIZE_CACHE_10;
      // Prefer writing to storage medium (instead of nv cache) and return only
      // after completion of operation.
      cdb.syncnv_immed = 0;
      // Ideally this would flush specific blocks, but several platforms don't
      // support this functionality, so just synchronize the whole disk.
      cdb.logical_block_address = 0;
      cdb.num_blocks = 0;
      controller_->ExecuteCommandAsync(target_, lun_, {&cdb, sizeof(cdb)},
                                       /*is_write=*/false, block_size_bytes_, disk_op);
      break;
    }
    default:
      completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, op);
      return;
  }
}

}  // namespace scsi
