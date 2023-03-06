// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sata.h"

#include <inttypes.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/sync/completion.h>
#include <lib/zx/vmo.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/param.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>

#include "controller.h"
#include "src/devices/block/lib/common/include/common.h"

namespace ahci {

constexpr size_t kQemuMaxTransferBlocks = 1024;  // Linux kernel limit

static void SataIdentifyDeviceComplete(void* cookie, zx_status_t status, block_op_t* op) {
  SataTransaction* txn = containerof(op, SataTransaction, bop);
  txn->status = status;
  sync_completion_signal(static_cast<sync_completion_t*>(cookie));
}

static bool IsModelIdQemu(char* model_id) {
  constexpr char kQemuModelId[] = "QEMU HARDDISK";
  return !memcmp(model_id, kQemuModelId, sizeof(kQemuModelId) - 1);
}

void SataDevice::DdkInit(ddk::InitTxn txn) {
  // The driver initialization has numerous error conditions. Wrap the initialization here to ensure
  // we always call txn.Reply() in any outcome.
  zx_status_t status = Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "sata: Driver initialization failed: %s", zx_status_get_string(status));
  }
  txn.Reply(status);
}

zx_status_t SataDevice::Init() {
  // Set default devinfo
  SataDeviceInfo di;
  di.block_size = 512;
  di.max_cmd = 1;
  controller_->SetDevInfo(port_, &di);

  // send IDENTIFY DEVICE
  zx::vmo vmo;
  zx_status_t status = zx::vmo::create(512, 0, &vmo);
  if (status != ZX_OK) {
    zxlogf(DEBUG, "sata: error %d allocating vmo", status);
    return status;
  }

  sync_completion_t completion;
  SataTransaction txn = {};
  txn.bop.rw.vmo = vmo.get();
  txn.bop.rw.length = 1;
  txn.bop.rw.offset_dev = 0;
  txn.bop.rw.offset_vmo = 0;
  txn.cmd = SATA_CMD_IDENTIFY_DEVICE;
  txn.device = 0;
  txn.completion_cb = SataIdentifyDeviceComplete;
  txn.cookie = &completion;

  controller_->Queue(port_, &txn);
  sync_completion_wait(&completion, ZX_TIME_INFINITE);

  if (txn.status != ZX_OK) {
    zxlogf(ERROR, "%s: error %d in device identify", DriverName().c_str(), txn.status);
    return txn.status;
  }

  // parse results
  SataIdentifyDeviceResponse devinfo;
  status = vmo.read(&devinfo, 0, sizeof(devinfo));
  if (status != ZX_OK) {
    zxlogf(ERROR, "sata: error %d in vmo_read", status);
    return ZX_ERR_INTERNAL;
  }
  vmo.reset();

  // Strings are 16-bit byte-flipped. Fix in place.
  // Strings are NOT null-terminated.
  SataStringFix(devinfo.serial.word, sizeof(devinfo.serial.word));
  SataStringFix(devinfo.firmware_rev.word, sizeof(devinfo.firmware_rev.word));
  SataStringFix(devinfo.model_id.word, sizeof(devinfo.model_id.word));

  zxlogf(INFO, "%s: dev info", DriverName().c_str());
  zxlogf(INFO, "  serial=%.*s", SATA_DEVINFO_SERIAL_LEN, devinfo.serial.string);
  zxlogf(INFO, "  firmware rev=%.*s", SATA_DEVINFO_FW_REV_LEN, devinfo.firmware_rev.string);
  zxlogf(INFO, "  model id=%.*s", SATA_DEVINFO_MODEL_ID_LEN, devinfo.model_id.string);

  bool is_qemu = IsModelIdQemu(devinfo.model_id.string);

  uint16_t major = devinfo.major_version;
  zxlogf(INFO, "  major=0x%x ", major);
  switch (32 - __builtin_clz(major) - 1) {
    case 11:
      zxlogf(INFO, "ACS4");
      break;
    case 10:
      zxlogf(INFO, "ACS3");
      break;
    case 9:
      zxlogf(INFO, "ACS2");
      break;
    case 8:
      zxlogf(INFO, "ATA8-ACS");
      break;
    case 7:
    case 6:
    case 5:
      zxlogf(INFO, "ATA/ATAPI");
      break;
    default:
      zxlogf(INFO, "Obsolete");
      break;
  }

  uint16_t cap = devinfo.capabilities_1;
  if (cap & (1 << 8)) {
    zxlogf(INFO, " DMA");
  } else {
    zxlogf(INFO, " PIO");
  }
  uint32_t max_cmd = devinfo.queue_depth;
  zxlogf(INFO, " %u commands", max_cmd + 1);

  uint32_t block_size = 512;  // default
  uint64_t block_count = 0;
  if (cap & (1 << 9)) {
    if ((devinfo.sector_size & 0xd000) == 0x5000) {
      block_size = 2 * devinfo.logical_sector_size;
    }
    if (devinfo.command_set1_1 & (1 << 10)) {
      block_count = devinfo.lba_capacity2;
      zxlogf(INFO, "  LBA48");
    } else {
      block_count = devinfo.lba_capacity;
      zxlogf(INFO, "  LBA");
    }
    zxlogf(INFO, " %" PRIu64 " sectors,  sector size=%u", block_count, block_size);
  } else {
    zxlogf(INFO, "  CHS unsupported!");
  }

  info_.block_size = block_size;
  info_.block_count = block_count;

  if (devinfo.command_set2_0 & SATA_DEVINFO_CMD_SET2_0_VOLATILE_WRITE_CACHE_ENABLED) {
    zxlogf(DEBUG, " Volatile write cache enabled");
  }
  if (devinfo.command_set1_0 & SATA_DEVINFO_CMD_SET1_0_VOLATILE_WRITE_CACHE_SUPPORTED) {
    zxlogf(DEBUG, " Volatile write cache supported");
  }
  if (devinfo.command_set1_2 & SATA_DEVINFO_CMD_SET1_2_WRITE_DMA_FUA_EXT_SUPPORTED) {
    zxlogf(DEBUG, " FUA command supported");
    info_.flags |= BLOCK_FLAG_FUA_SUPPORT;
  }

  uint32_t max_sg_size = SATA_MAX_BLOCK_COUNT * block_size;  // SATA cmd limit
  if (is_qemu) {
    max_sg_size = MIN(max_sg_size, kQemuMaxTransferBlocks * block_size);
  }
  info_.max_transfer_size = MIN(AHCI_MAX_BYTES, max_sg_size);

  // set devinfo on controller
  di.block_size = block_size;
  di.max_cmd = max_cmd;
  controller_->SetDevInfo(port_, &di);

  return ZX_OK;
}

// implement device protocol:

void SataDevice::DdkRelease() { delete this; }

void SataDevice::BlockImplQuery(block_info_t* info_out, uint64_t* block_op_size_out) {
  *info_out = info_;
  *block_op_size_out = sizeof(SataTransaction);
}

void SataDevice::BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb,
                                void* cookie) {
  SataTransaction* txn = containerof(bop, SataTransaction, bop);
  txn->completion_cb = completion_cb;
  txn->cookie = cookie;

  switch (BLOCK_OP(bop->command)) {
    case BLOCK_OP_READ:
    case BLOCK_OP_WRITE:
      if (zx_status_t status = block::CheckIoRange(bop->rw, info_.block_count); status != ZX_OK) {
        txn->Complete(status);
        return;
      }

      if (BLOCK_OP(bop->command) == BLOCK_OP_READ) {
        txn->cmd = SATA_CMD_READ_DMA_EXT;
      } else {
        txn->cmd = (BLOCK_FLAGS(bop->command) & BLOCK_FL_FORCE_ACCESS) ? SATA_CMD_WRITE_DMA_FUA_EXT
                                                                       : SATA_CMD_WRITE_DMA_EXT;
      }
      txn->device = 0x40;

      zxlogf(DEBUG, "sata: queue op 0x%x txn %p", bop->command, txn);
      break;
    case BLOCK_OP_FLUSH:
      zxlogf(DEBUG, "sata: queue FLUSH txn %p", txn);
      break;
    default:
      txn->Complete(ZX_ERR_NOT_SUPPORTED);
      return;
  }

  controller_->Queue(port_, txn);
}

zx_status_t SataDevice::Bind(Controller* controller, uint32_t port) {
  // initialize the device
  fbl::AllocChecker ac;
  auto device = fbl::make_unique_checked<SataDevice>(&ac, controller->zxdev(), controller, port);
  if (!ac.check()) {
    zxlogf(ERROR, "sata: Failed to allocate memory for SATA device at port %u.", port);
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status = device->AddDriver();
  if (status != ZX_OK) {
    return status;
  }

  // The DriverFramework now owns driver.
  device.release();
  return ZX_OK;
}

zx_status_t SataDevice::AddDriver() { return DdkAdd(DriverName().c_str()); }

}  // namespace ahci
