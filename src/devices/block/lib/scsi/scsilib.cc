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

uint32_t CountLuns(Controller* controller, uint8_t target) {
  ReportLunsParameterDataHeader data = {};

  ReportLunsCDB cdb = {};
  cdb.opcode = Opcode::REPORT_LUNS;
  cdb.allocation_length = htonl(sizeof(data));

  auto status = controller->ExecuteCommandSync(target, 0, {&cdb, sizeof(cdb)}, /*is_write=*/false,
                                               {&data, sizeof(data)});
  if (status != ZX_OK) {
    // For now, assume REPORT LUNS is supported. A failure indicates no LUNs on this target.
    return 0;
  }
  // data.lun_list_length is the number of bytes of LUN structures.
  return ntohl(data.lun_list_length) / 8;
}

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
  InquiryCDB inquiry_cdb = {};
  InquiryData inquiry_data = {};
  inquiry_cdb.opcode = Opcode::INQUIRY;
  inquiry_cdb.allocation_length = ntohs(sizeof(inquiry_data));

  auto status =
      controller_->ExecuteCommandSync(target_, lun_, {&inquiry_cdb, sizeof(inquiry_cdb)},
                                      /*is_write=*/false, {&inquiry_data, sizeof(inquiry_data)});
  if (status != ZX_OK) {
    return status;
  }
  // Check that its a disk first.
  if (inquiry_data.peripheral_device_type != 0) {
    return ZX_ERR_IO;
  }

  // Print T10 Vendor ID/Product ID
  zxlogf(INFO, "%d:%d ", target_, lun_);
  for (int i = 0; i < 8; i++) {
    zxlogf(INFO, "%c", inquiry_data.t10_vendor_id[i]);
  }
  zxlogf(INFO, " ");
  for (int i = 0; i < 16; i++) {
    zxlogf(INFO, "%c", inquiry_data.product_id[i]);
  }
  zxlogf(INFO, "");

  removable_ = inquiry_data.removable_media();

  // TODO(fxbug.dev/118937): Check for write-protected mode.

  // Determine if the disk has write cache enabled.
  ModeSense6CDB mode_sense_cdb = {};
  CachingModePage caching_mode_page = {};
  mode_sense_cdb.opcode = Opcode::MODE_SENSE_6;
  // Only fetch the caching mode page and get the current values.
  mode_sense_cdb.page_code = kCachingPageCode;
  mode_sense_cdb.allocation_length = sizeof(caching_mode_page);
  // Do not return any block descriptors.
  mode_sense_cdb.disable_block_descriptors = 0b1000;
  status = controller_->ExecuteCommandSync(target_, lun_, {&mode_sense_cdb, sizeof(mode_sense_cdb)},
                                           /*is_write=*/false,
                                           {&caching_mode_page, sizeof(caching_mode_page)});
  if (status != ZX_OK) {
    return status;
  }
  if (caching_mode_page.page_code != kCachingPageCode) {
    zxlogf(ERROR, "failed to retrieve caching mode page");
    return ZX_ERR_INTERNAL;
  }
  write_cache_enabled_ = caching_mode_page.write_cache_enabled();

  ReadCapacity16CDB read_capacity_cdb = {};
  ReadCapacity16ParameterData read_capacity_data = {};
  read_capacity_cdb.opcode = Opcode::READ_CAPACITY_16;
  read_capacity_cdb.service_action = 0x10;
  read_capacity_cdb.allocation_length = ntohl(sizeof(read_capacity_data));
  status = controller_->ExecuteCommandSync(
      target_, lun_, {&read_capacity_cdb, sizeof(read_capacity_cdb)}, /*is_write=*/false,
      {&read_capacity_data, sizeof(read_capacity_data)});
  if (status != ZX_OK) {
    return status;
  }

  block_count_ = htobe64(read_capacity_data.returned_logical_block_address) + 1;
  block_size_bytes_ = ntohl(read_capacity_data.block_length_in_bytes);

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
  // TODO(fxbug.dev/118937): Report read-only flag as appropriate.
  info_out->flags = removable_ ? BLOCK_FLAG_REMOVABLE : 0;
  *block_op_size_out = controller_->BlockOpSize();
}

void Disk::BlockImplQueue(block_op_t* op, block_impl_queue_callback completion_cb, void* cookie) {
  ScsiLibOp* scsilib_op = containerof(op, ScsiLibOp, op);
  auto op_type = scsilib_op->op.command & BLOCK_OP_MASK;
  scsilib_op->completion_cb = completion_cb;
  scsilib_op->cookie = cookie;

  switch (op_type) {
    case BLOCK_OP_READ: {
      Read16CDB cdb = {};
      cdb.opcode = Opcode::READ_16;
      cdb.logical_block_address = htobe64(scsilib_op->op.rw.offset_dev);
      cdb.transfer_length = htonl(scsilib_op->op.rw.length);
      controller_->ExecuteCommandAsync(target_, lun_, {&cdb, sizeof(cdb)},
                                       /*is_write=*/false, block_size_bytes_, scsilib_op);
      break;
    }
    case BLOCK_OP_WRITE: {
      Write16CDB cdb = {};
      cdb.opcode = Opcode::WRITE_16;
      if (scsilib_op->op.command & BLOCK_FL_FORCE_ACCESS) {
        cdb.set_force_unit_access(true);
      }
      cdb.logical_block_address = htobe64(scsilib_op->op.rw.offset_dev);
      cdb.transfer_length = htonl(scsilib_op->op.rw.length);
      controller_->ExecuteCommandAsync(target_, lun_, {&cdb, sizeof(cdb)},
                                       /*is_write=*/true, block_size_bytes_, scsilib_op);
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
                                       /*is_write=*/false, block_size_bytes_, scsilib_op);
      break;
    }
    default:
      completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, op);
      return;
  }
}

}  // namespace scsi
