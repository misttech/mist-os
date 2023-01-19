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

  auto status = controller->ExecuteCommandSync(/*target=*/target, /*lun=*/0,
                                               /*cdb=*/{&cdb, sizeof(cdb)},
                                               /*data_out=*/{nullptr, 0},
                                               /*data_in=*/{&data, sizeof(data)});
  if (status != ZX_OK) {
    // For now, assume REPORT LUNS is supported. A failure indicates no LUNs on this target.
    return 0;
  }
  // data.lun_list_length is the number of bytes of LUN structures.
  return ntohl(data.lun_list_length) / 8;
}

zx_status_t Disk::Create(Controller* controller, zx_device_t* parent, uint8_t target, uint16_t lun,
                         uint32_t max_xfer_size) {
  fbl::AllocChecker ac;
  auto* const disk = new (&ac) scsi::Disk(controller, parent, /*target=*/target, /*lun=*/lun);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  disk->max_xfer_size_ = max_xfer_size;
  auto status = disk->Bind();
  if (status != ZX_OK) {
    delete disk;
  }
  return status;
}

zx_status_t Disk::Bind() {
  InquiryCDB inquiry_cdb = {};
  InquiryData inquiry_data = {};
  inquiry_cdb.opcode = Opcode::INQUIRY;
  inquiry_cdb.allocation_length = ntohs(sizeof(inquiry_data));

  auto status = controller_->ExecuteCommandSync(/*target=*/target_, /*lun=*/lun_,
                                                /*cdb=*/{&inquiry_cdb, sizeof(inquiry_cdb)},
                                                /*data_out=*/{nullptr, 0},
                                                /*data_in=*/{&inquiry_data, sizeof(inquiry_data)});
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

  removable_ = (inquiry_data.removable & 0x80);

  // Determine if the disk has write cache enabled.
  ModeSense6CDB mode_sense_cdb = {};
  CachingModePage caching_mode_page = {};
  mode_sense_cdb.opcode = Opcode::MODE_SENSE_6;
  // Only fetch the caching mode page and get the current values.
  mode_sense_cdb.page_code = kCachingPageCode;
  mode_sense_cdb.allocation_length = sizeof(caching_mode_page);
  // Do not return any block descriptors.
  mode_sense_cdb.disable_block_descriptors = 0b1000;
  status = controller_->ExecuteCommandSync(
      /*target=*/target_, /*lun=*/lun_,
      /*cdb=*/{&mode_sense_cdb, sizeof(mode_sense_cdb)},
      /*data_out=*/{nullptr, 0},
      /*data_in=*/{&caching_mode_page, sizeof(caching_mode_page)});
  if (status != ZX_OK) {
    return status;
  }
  if (caching_mode_page.page_code != kCachingPageCode) {
    zxlogf(ERROR, "failed to retrieve caching mode page");
    return ZX_ERR_INTERNAL;
  }
  write_cache_enabled_ = caching_mode_page.control_bits & 0b100;

  ReadCapacity16CDB read_capacity_cdb = {};
  ReadCapacity16ParameterData read_capacity_data = {};
  read_capacity_cdb.opcode = Opcode::READ_CAPACITY_16;
  read_capacity_cdb.service_action = 0x10;
  read_capacity_cdb.allocation_length = ntohl(sizeof(read_capacity_data));
  status = controller_->ExecuteCommandSync(
      /*target=*/target_, /*lun=*/lun_,
      /*cdb=*/{&read_capacity_cdb, sizeof(read_capacity_cdb)},
      /*data_out=*/{nullptr, 0},
      /*data_in=*/{&read_capacity_data, sizeof(read_capacity_data)});
  if (status != ZX_OK) {
    return status;
  }

  blocks_ = htobe64(read_capacity_data.returned_logical_block_address) + 1;
  block_size_ = ntohl(read_capacity_data.block_length_in_bytes);

  zxlogf(INFO, "%ld blocks of %d bytes", blocks_, block_size_);

  return DdkAdd(tag_);
}

struct ScsiLibOp {
  block_op_t op;
  block_impl_queue_callback completion_cb;
  void* cookie;
  uint32_t block_size;
  zx_vaddr_t mapped_addr;
  void* data;
};

static void ScsiLibCompletionCb(void* c, zx_status_t status) {
  auto scsilib_op = reinterpret_cast<ScsiLibOp*>(c);
  uint64_t length = scsilib_op->op.rw.length * scsilib_op->block_size;
  uint64_t vmo_offset = scsilib_op->op.rw.offset_vmo * scsilib_op->block_size;

  if (scsilib_op->mapped_addr != reinterpret_cast<zx_vaddr_t>(nullptr)) {
    status = zx_vmar_unmap(zx_vmar_root_self(), scsilib_op->mapped_addr, length);
  } else {
    auto op_type = scsilib_op->op.command & BLOCK_OP_MASK;

    if (op_type == BLOCK_OP_READ) {
      if (status == ZX_OK) {
        status = zx_vmo_write(scsilib_op->op.rw.vmo, scsilib_op->data, vmo_offset, length);
      }
    }
    free(scsilib_op->data);
  }
  scsilib_op->completion_cb(scsilib_op->cookie, status, &scsilib_op->op);
}

void Disk::BlockImplQuery(block_info_t* info_out, size_t* block_op_size_out) {
  info_out->block_size = block_size_;
  info_out->block_count = blocks_;
  info_out->max_transfer_size = block_size_ * max_xfer_size_;
  info_out->flags = (removable_) ? BLOCK_FLAG_REMOVABLE : 0;
  *block_op_size_out = sizeof(ScsiLibOp);
}

void Disk::BlockImplQueue(block_op_t* op, block_impl_queue_callback completion_cb, void* cookie) {
  ScsiLibOp* scsilib_op = containerof(op, ScsiLibOp, op);
  auto op_type = scsilib_op->op.command & BLOCK_OP_MASK;

  // To use zx_vmar_map, offset, length must be page aligned. If it isn't (uncommon),
  // allocate a temp buffer and do a copy.
  uint64_t length = scsilib_op->op.rw.length * block_size_;
  uint64_t vmo_offset = scsilib_op->op.rw.offset_vmo * block_size_;
  zx_vaddr_t mapped_addr = reinterpret_cast<zx_vaddr_t>(nullptr);
  void* data = nullptr;  // Quiet compiler.
  zx_status_t status;
  if ((length > 0) && ((length % zx_system_get_page_size()) == 0) &&
      ((vmo_offset % zx_system_get_page_size()) == 0)) {
    status = zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0,
                         scsilib_op->op.rw.vmo, vmo_offset, length, &mapped_addr);
    if (status != ZX_OK) {
      completion_cb(cookie, status, op);
      return;
    }
    data = reinterpret_cast<void*>(mapped_addr);
  } else {
    data = calloc(scsilib_op->op.rw.length, block_size_);
    if (op_type == BLOCK_OP_WRITE) {
      status = zx_vmo_read(scsilib_op->op.rw.vmo, data, vmo_offset, length);
      if (status != ZX_OK) {
        free(data);
        completion_cb(cookie, status, op);
        return;
      }
    }
  }

  scsilib_op->completion_cb = completion_cb;
  scsilib_op->cookie = cookie;
  scsilib_op->block_size = block_size_;
  scsilib_op->mapped_addr = mapped_addr;
  scsilib_op->data = data;

  switch (op_type) {
    case BLOCK_OP_READ: {
      Read16CDB cdb = {};
      cdb.opcode = Opcode::READ_16;
      cdb.logical_block_address = htobe64(scsilib_op->op.rw.offset_dev);
      cdb.transfer_length = htonl(scsilib_op->op.rw.length);
      status = controller_->ExecuteCommandAsync(/*target=*/target_, /*lun=*/lun_,
                                                /*cdb=*/{&cdb, sizeof(cdb)},
                                                /*data_out=*/{nullptr, 0},
                                                /*data_in=*/{data, length}, ScsiLibCompletionCb,
                                                scsilib_op);
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
      status = controller_->ExecuteCommandAsync(/*target=*/target_, /*lun=*/lun_,
                                                /*cdb=*/{&cdb, sizeof(cdb)},
                                                /*data_out=*/{data, length},
                                                /*data_in=*/{nullptr, 0}, ScsiLibCompletionCb,
                                                scsilib_op);
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
      status = controller_->ExecuteCommandAsync(/*target=*/target_, /*lun=*/lun_,
                                                /*cdb=*/{&cdb, sizeof(cdb)},
                                                /*data_out=*/{nullptr, 0},
                                                /*data_in=*/{nullptr, 0}, ScsiLibCompletionCb,
                                                scsilib_op);
      break;
    }
    default:
      completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, op);
      return;
  }
}

Disk::Disk(Controller* controller, zx_device_t* parent, uint8_t target, uint16_t lun)
    : DeviceType(parent), controller_(controller), target_(target), lun_(lun) {
  snprintf(tag_, sizeof(tag_), "scsi-disk-%d-%d", target_, lun_);
}

}  // namespace scsi
