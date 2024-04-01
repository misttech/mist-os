// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sata.h"

#include <inttypes.h>
#include <lib/ddk/binding_driver.h>
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
  // Use the 32-bit command field to shuttle the status back to the callsite that's waiting on the
  // completion. This works despite the int32_t (zx_status_t) vs. uint32_t (command) mismatch.
  op->command.flags = status;
  sync_completion_signal(static_cast<sync_completion_t*>(cookie));
}

static bool IsModelIdQemu(char* model_id) {
  constexpr char kQemuModelId[] = "QEMU HARDDISK";
  return !memcmp(model_id, kQemuModelId, sizeof(kQemuModelId) - 1);
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
    FDF_LOG(ERROR, "Failed to allocate vmo: %s", zx_status_get_string(status));
    return status;
  }

  sync_completion_t completion;
  SataTransaction txn = {};
  txn.bop.rw.command.opcode = BLOCK_OPCODE_READ;
  txn.bop.rw.vmo = vmo.get();
  txn.bop.rw.length = 1;
  txn.bop.rw.offset_dev = 0;
  txn.bop.rw.offset_vmo = 0;
  txn.completion_cb = SataIdentifyDeviceComplete;
  txn.cookie = &completion;
  txn.cmd = SATA_CMD_IDENTIFY_DEVICE;
  txn.device = 0;

  controller_->Queue(port_, &txn);
  sync_completion_wait(&completion, ZX_TIME_INFINITE);

  status = txn.bop.command.flags;
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "%s: Failed IDENTIFY_DEVICE: %s", DriverName().c_str(),
            zx_status_get_string(status));
    return status;
  }

  // parse results
  SataIdentifyDeviceResponse devinfo;
  status = vmo.read(&devinfo, 0, sizeof(devinfo));
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed vmo_read: %s", zx_status_get_string(status));
    return ZX_ERR_INTERNAL;
  }
  vmo.reset();

  // Strings are 16-bit byte-flipped. Fix in place.
  // Strings are NOT null-terminated.
  SataStringFix(devinfo.serial.word, sizeof(devinfo.serial.word));
  SataStringFix(devinfo.firmware_rev.word, sizeof(devinfo.firmware_rev.word));
  SataStringFix(devinfo.model_id.word, sizeof(devinfo.model_id.word));

  auto model_number = std::string(devinfo.model_id.string, sizeof(devinfo.model_id.string));
  auto serial_number = std::string(devinfo.serial.string, sizeof(devinfo.serial.string));
  auto firmware_rev = std::string(devinfo.firmware_rev.string, sizeof(devinfo.firmware_rev.string));
  // Some vendors don't pad the strings with spaces (0x20). Null-terminate strings to avoid printing
  // illegal characters.
  model_number = std::string(model_number.c_str());
  serial_number = std::string(serial_number.c_str());
  firmware_rev = std::string(firmware_rev.c_str());
  FDF_LOG(INFO, "Model number:  '%s'", model_number.c_str());
  FDF_LOG(INFO, "Serial number: '%s'", serial_number.c_str());
  FDF_LOG(INFO, "Firmware rev.: '%s'", firmware_rev.c_str());

  auto inspect_device = controller_->inspect_node().CreateChild(DriverName());
  inspect_device.RecordString("model_number", model_number);
  inspect_device.RecordString("serial_number", serial_number);
  inspect_device.RecordString("firmware_rev", firmware_rev);

  switch (32 - __builtin_clz(devinfo.major_version) - 1) {
    case 11:
      inspect_device.RecordString("major_version", "ACS4");
      break;
    case 10:
      inspect_device.RecordString("major_version", "ACS3");
      break;
    case 9:
      inspect_device.RecordString("major_version", "ACS2");
      break;
    case 8:
      inspect_device.RecordString("major_version", "ATA8-ACS");
      break;
    case 7:
    case 6:
    case 5:
      inspect_device.RecordString("major_version", "ATA/ATAPI");
      break;
    default:
      inspect_device.RecordString("major_version", "Obsolete");
      break;
  }

  uint16_t cap = devinfo.capabilities_1;
  if (cap & (1 << 8)) {
    inspect_device.RecordString("capabilities", "DMA");
  } else {
    inspect_device.RecordString("capabilities", "PIO");
  }
  uint32_t max_cmd = devinfo.queue_depth;
  inspect_device.RecordUint("max_commands", max_cmd + 1);

  uint32_t block_size = 512;  // default
  uint64_t block_count = 0;
  if (cap & (1 << 9)) {
    if ((devinfo.sector_size & 0xd000) == 0x5000) {
      block_size = 2 * devinfo.logical_sector_size;
    }
    if (devinfo.command_set1_1 & (1 << 10)) {
      block_count = devinfo.lba_capacity2;
      inspect_device.RecordString("addressing", "48-bit LBA");
    } else {
      block_count = devinfo.lba_capacity;
      inspect_device.RecordString("addressing", "28-bit LBA");
    }
    inspect_device.RecordUint("sector_count", block_count);
    inspect_device.RecordUint("sector_size", block_size);
  } else {
    inspect_device.RecordString("addressing", "CHS unsupported");
  }

  info_.block_size = block_size;
  info_.block_count = block_count;

  const bool volatile_write_cache_supported =
      devinfo.command_set1_0 & SATA_DEVINFO_CMD_SET1_0_VOLATILE_WRITE_CACHE_SUPPORTED;
  const bool volatile_write_cache_enabled =
      devinfo.command_set2_0 & SATA_DEVINFO_CMD_SET2_0_VOLATILE_WRITE_CACHE_ENABLED;
  inspect_device.RecordBool("volatile_write_cache_supported", volatile_write_cache_supported);
  inspect_device.RecordBool("volatile_write_cache_enabled", volatile_write_cache_enabled);

  // READ_FPDMA_QUEUED and WRITE_FPDMA_QUEUED commands support FUA, whereas for non-NCQ, FUA read
  // commands do not exist (FUA writes do).
  if (use_command_queue_) {
    info_.flags |= FLAG_FUA_SUPPORT;
  }

  uint32_t max_sg_size = SATA_MAX_BLOCK_COUNT * block_size;  // SATA cmd limit
  if (IsModelIdQemu(devinfo.model_id.string)) {
    max_sg_size = MIN(max_sg_size, kQemuMaxTransferBlocks * block_size);
  }
  info_.max_transfer_size = MIN(AHCI_MAX_BYTES, max_sg_size);

  // set devinfo on controller
  di.block_size = block_size;
  di.max_cmd = max_cmd;
  controller_->SetDevInfo(port_, &di);

  controller_->inspector().emplace(std::move(inspect_device));
  return ZX_OK;
}

// implement device protocol:

void SataDevice::BlockImplQuery(block_info_t* info_out, uint64_t* block_op_size_out) {
  *info_out = info_;
  *block_op_size_out = sizeof(SataTransaction);
}

void SataDevice::BlockImplQueue(block_op_t* bop, block_impl_queue_callback completion_cb,
                                void* cookie) {
  SataTransaction* txn = containerof(bop, SataTransaction, bop);
  txn->completion_cb = completion_cb;
  txn->cookie = cookie;

  switch (bop->command.opcode) {
    case BLOCK_OPCODE_READ:
    case BLOCK_OPCODE_WRITE: {
      if (zx_status_t status = block::CheckIoRange(bop->rw, info_.block_count, logger());
          status != ZX_OK) {
        txn->Complete(status);
        return;
      }

      txn->device = 0x40;
      const bool is_read = bop->command.opcode == BLOCK_OPCODE_READ;
      const bool is_fua = bop->command.flags & BLOCK_IO_FLAG_FORCE_ACCESS;
      if (use_command_queue_) {
        if (is_fua) {
          txn->device |= 1 << 7;  // Set FUA
        }
        txn->cmd = is_read ? SATA_CMD_READ_FPDMA_QUEUED : SATA_CMD_WRITE_FPDMA_QUEUED;
      } else {
        if (is_fua) {
          txn->Complete(ZX_ERR_NOT_SUPPORTED);
          return;
        }
        txn->cmd = is_read ? SATA_CMD_READ_DMA_EXT : SATA_CMD_WRITE_DMA_EXT;
      }

      FDF_LOG(DEBUG, "Queue op 0x%x txn %p", bop->command.opcode, txn);
      break;
    }
    case BLOCK_OPCODE_FLUSH:
      txn->cmd = SATA_CMD_FLUSH_EXT;
      txn->device = 0x00;
      FDF_LOG(DEBUG, "Queue FLUSH txn %p", txn);
      break;
    default:
      txn->Complete(ZX_ERR_NOT_SUPPORTED);
      return;
  }

  controller_->Queue(port_, txn);
}

zx::result<std::unique_ptr<SataDevice>> SataDevice::Bind(Controller* controller, uint32_t port,
                                                         bool use_command_queue) {
  // initialize the device
  fbl::AllocChecker ac;
  auto device = fbl::make_unique_checked<SataDevice>(&ac, controller, port, use_command_queue);
  if (!ac.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for SATA device at port %u.", port);
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx_status_t status = device->AddDevice();
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(device));
}

zx_status_t SataDevice::AddDevice() {
  {
    const std::string path_from_parent = std::string(controller_->driver_name()) + "/";
    compat::DeviceServer::BanjoConfig banjo_config;
    banjo_config.callbacks[ZX_PROTOCOL_BLOCK_IMPL] = block_impl_server_.callback();

    auto result = compat_server_.Initialize(
        controller_->driver_incoming(), controller_->driver_outgoing(),
        controller_->driver_node_name(), DriverName(), compat::ForwardMetadata::None(),
        std::move(banjo_config), path_from_parent);
    if (result.is_error()) {
      return result.status_value();
    }
  }

  zx_status_t status = Init();
  if (status != ZX_OK) {
    return status;
  }

  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  node_controller_.Bind(std::move(controller_client_end));

  fidl::Arena arena;

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 1);
  properties[0] = fdf::MakeProperty(arena, BIND_PROTOCOL, ZX_PROTOCOL_BLOCK_IMPL);

  std::vector<fuchsia_driver_framework::wire::Offer> offers = compat_server_.CreateOffers2(arena);

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, DriverName())
                        .offers2(arena, std::move(offers))
                        .properties(properties)
                        .Build();

  auto result = controller_->root_node()->AddChild(args, std::move(controller_server_end), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child SATA device: %s", result.status_string());
    return result.status();
  }
  return ZX_OK;
}

fdf::Logger& SataDevice::logger() { return controller_->logger(); }

}  // namespace ahci
