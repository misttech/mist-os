// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <endian.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/scsi/disk.h>
#include <netinet/in.h>
#include <zircon/process.h>

#include <fbl/alloc_checker.h>

#include "src/devices/block/lib/common/include/common.h"

namespace scsi {

zx::result<fbl::RefPtr<Disk>> Disk::Bind(zx_device_t* parent, Controller* controller,
                                         uint8_t target, uint16_t lun,
                                         uint32_t max_transfer_bytes) {
  fbl::AllocChecker ac;
  auto disk =
      fbl::MakeRefCountedChecked<Disk>(&ac, parent, controller, target, lun, max_transfer_bytes);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  auto status = disk->AddDisk();
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(disk);
}

zx_status_t Disk::AddDisk() {
  zx::result inquiry_data = controller_->Inquiry(target_, lun_);
  if (inquiry_data.is_error()) {
    return inquiry_data.status_value();
  }
  // Check that its a disk first.
  if (inquiry_data.value().peripheral_device_type != 0) {
    return ZX_ERR_IO;
  }

  // Print T10 Vendor ID/Product ID
  auto vendor_id =
      std::string(inquiry_data.value().t10_vendor_id, sizeof(inquiry_data.value().t10_vendor_id));
  auto product_id =
      std::string(inquiry_data.value().product_id, sizeof(inquiry_data.value().product_id));
  // Some vendors don't pad the strings with spaces (0x20). Null-terminate strings to avoid printing
  // illegal characters.
  vendor_id = std::string(vendor_id.c_str());
  product_id = std::string(product_id.c_str());
  zxlogf(INFO, "Target %u LUN %u: Vendor ID = %s, Product ID = %s", target_, lun_,
         vendor_id.c_str(), product_id.c_str());

  removable_ = inquiry_data.value().removable_media();

  zx::result mode_sense_data = controller_->ModeSense(target_, lun_);
  if (mode_sense_data.is_error()) {
    return mode_sense_data.status_value();
  }
  dpo_fua_available_ = mode_sense_data.value().dpo_fua_available();
  write_protected_ = mode_sense_data.value().write_protected();

  zx::result write_cache_enabled = controller_->ModeSenseWriteCacheEnabled(target_, lun_);
  if (write_cache_enabled.is_error()) {
    zxlogf(WARNING, "Failed to get write cache status for target %u, lun %u: %s.", target_, lun_,
           zx_status_get_string(write_cache_enabled.status_value()));
    // Assume write cache is enabled so that flush operations are not ignored.
    write_cache_enabled_ = true;
  } else {
    write_cache_enabled_ = write_cache_enabled.value();
  }

  zx_status_t status = controller_->ReadCapacity(target_, lun_, &block_count_, &block_size_bytes_);
  if (status != ZX_OK) {
    return status;
  }

  if (max_transfer_bytes_ == fuchsia_hardware_block::wire::kMaxTransferUnbounded) {
    max_transfer_blocks_ = UINT32_MAX;
  } else {
    if (max_transfer_bytes_ % block_size_bytes_ != 0) {
      zxlogf(ERROR, "Max transfer size (%u bytes) is not a multiple of the block size (%u bytes).",
             max_transfer_bytes_, block_size_bytes_);
      return ZX_ERR_BAD_STATE;
    }
    max_transfer_blocks_ = max_transfer_bytes_ / block_size_bytes_;
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
  info_out->max_transfer_size = max_transfer_bytes_;
  info_out->flags = (write_protected_ ? FLAG_READONLY : 0) | (removable_ ? FLAG_REMOVABLE : 0) |
                    (dpo_fua_available_ ? FLAG_FUA_SUPPORT : 0);
  *block_op_size_out = controller_->BlockOpSize();
}

void Disk::BlockImplQueue(block_op_t* op, block_impl_queue_callback completion_cb, void* cookie) {
  DiskOp* disk_op = containerof(op, DiskOp, op);
  disk_op->completion_cb = completion_cb;
  disk_op->cookie = cookie;

  switch (op->command.opcode) {
    case BLOCK_OPCODE_READ:
    case BLOCK_OPCODE_WRITE: {
      if (zx_status_t status = block::CheckIoRange(op->rw, block_count_, max_transfer_blocks_);
          status != ZX_OK) {
        completion_cb(cookie, status, op);
        return;
      }
      const bool is_write = op->command.opcode == BLOCK_OPCODE_WRITE;
      const bool is_fua = op->command.flags & BLOCK_IO_FLAG_FORCE_ACCESS;
      if (!dpo_fua_available_ && is_fua) {
        completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, op);
        return;
      }

      uint8_t cdb_buffer[16] = {};
      uint8_t cdb_length;
      if (block_count_ > UINT32_MAX) {
        auto cdb = reinterpret_cast<Read16CDB*>(cdb_buffer);  // Struct-wise equiv. to Write16CDB.
        cdb_length = 16;
        cdb->opcode = is_write ? Opcode::WRITE_16 : Opcode::READ_16;
        cdb->logical_block_address = htobe64(op->rw.offset_dev);
        cdb->transfer_length = htobe32(op->rw.length);
        cdb->set_force_unit_access(is_fua);
      } else if (op->rw.length <= UINT16_MAX) {
        auto cdb = reinterpret_cast<Read10CDB*>(cdb_buffer);  // Struct-wise equiv. to Write10CDB.
        cdb_length = 10;
        cdb->opcode = is_write ? Opcode::WRITE_10 : Opcode::READ_10;
        cdb->logical_block_address = htobe32(static_cast<uint32_t>(op->rw.offset_dev));
        cdb->transfer_length = htobe16(static_cast<uint16_t>(op->rw.length));
        cdb->set_force_unit_access(is_fua);
      } else {
        auto cdb = reinterpret_cast<Read12CDB*>(cdb_buffer);  // Struct-wise equiv. to Write12CDB.
        cdb_length = 12;
        cdb->opcode = is_write ? Opcode::WRITE_12 : Opcode::READ_12;
        cdb->logical_block_address = htobe32(static_cast<uint32_t>(op->rw.offset_dev));
        cdb->transfer_length = htobe32(op->rw.length);
        cdb->set_force_unit_access(is_fua);
      }
      ZX_ASSERT(cdb_length <= sizeof(cdb_buffer));
      controller_->ExecuteCommandAsync(target_, lun_, {cdb_buffer, cdb_length}, is_write,
                                       block_size_bytes_, disk_op);
      return;
    }
    case BLOCK_OPCODE_FLUSH: {
      if (zx_status_t status = block::CheckFlushValid(op->rw); status != ZX_OK) {
        completion_cb(cookie, status, op);
        return;
      }
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
      return;
    }
    default:
      completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, op);
      return;
  }
}

}  // namespace scsi
