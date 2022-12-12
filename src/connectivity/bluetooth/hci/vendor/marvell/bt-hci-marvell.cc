// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/hci/vendor/marvell/bt-hci-marvell.h"

#include <lib/async/cpp/task.h>
#include <lib/ddk/driver.h>

#include <fbl/alloc_checker.h>

#include "src/connectivity/bluetooth/hci/vendor/marvell/bt-hci-marvell-bind.h"

namespace bt_hci_marvell {

zx_status_t BtHciMarvell::Bind(void* ctx, zx_device_t* parent) {
  zx_status_t status;
  ddk::SdioProtocolClient sdio(parent);

  if (!sdio.is_valid()) {
    zxlogf(ERROR, "Failed to get SDIO protocol");
    return ZX_ERR_NO_RESOURCES;
  }

  // Allocate our driver instance
  fbl::AllocChecker ac;
  std::unique_ptr<BtHciMarvell> device(new (&ac) BtHciMarvell(parent, sdio));
  if (!ac.check()) {
    zxlogf(ERROR, "BtHciMarvell alloc failed");
    return ZX_ERR_NO_MEMORY;
  }

  // Add our device to the device tree
  ddk::DeviceAddArgs args("bt-hci-marvell");
  args.set_proto_id(ZX_PROTOCOL_BT_HCI);
  if ((status = device->DdkAdd(args)) != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed: %s", zx_status_get_string(status));
    return status;
  }

  // Driver Manager now owns the device - memory will be explicitly freed in DdkRelease()
  device.release();

  return ZX_OK;
}

void BtHciMarvell::DdkInit(ddk::InitTxn txn) {
  // Start up an independent driver thread
  loop_.emplace(&kAsyncLoopConfigNoAttachToCurrentThread);
  zx_status_t status = loop_->StartThread("bt-hci-marvell");
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to start thread: %s", zx_status_get_string(status));
    OnInitComplete(status, std::move(txn));
    return;
  }
  dispatcher_ = loop_->dispatcher();

  // Continue initialization in the new thread.
  async::PostTask(dispatcher_, [this, txn = std::move(txn)]() mutable {
    zx_status_t status = Init();
    OnInitComplete(status, std::move(txn));
  });
}

zx_status_t BtHciMarvell::Init() {
  zx_status_t status;
  sdio_hw_info_t hw_info;

  // Retrieve our product ID
  if ((status = sdio_.GetDevHwInfo(&hw_info)) != ZX_OK) {
    zxlogf(ERROR, "Failed to find SDIO hardware info: %s", zx_status_get_string(status));
    return status;
  }
  uint32_t product_id = hw_info.func_hw_info.product_id;

  // Determine if this device is supported. If so, instantiate our oracle.
  zx::result result = DeviceOracle::Create(product_id);
  if (result.is_error()) {
    zxlogf(ERROR, "Unable to find supported device matching product id %0x" PRIu32, product_id);
    return result.error_value();
  }
  device_oracle_ = std::move(result.value());

  if ((status = sdio_.EnableFn()) != ZX_OK) {
    zxlogf(ERROR, "Failed to enable SDIO function: %s", zx_status_get_string(status));
    return status;
  }

  // Set the SDIO block size
  uint16_t block_size = device_oracle_->GetSdioBlockSize();
  if ((status = sdio_.UpdateBlockSize(block_size, false)) != ZX_OK) {
    zxlogf(ERROR, "Failed to set SDIO block size to %u: %s", block_size,
           zx_status_get_string(status));
    return status;
  }

  if ((status = LoadFirmware()) != ZX_OK) {
    zxlogf(ERROR, "Failed to load firmware: %s", zx_status_get_string(status));
    return status;
  }

  // Retrieve the address of the IO Port, which is where we will perform read/writes of HCI packets
  uint32_t ioport_addr_reg_addr = device_oracle_->GetRegAddrIoportAddr();
  if ((status = Read24(ioport_addr_reg_addr, &ioport_addr_)) != ZX_OK) {
    return status;
  }
  zxlogf(INFO, "IO port address: %#x", ioport_addr_);

  // Set interrupt behavior to "clear-on-read"
  uint8_t rsr = 0xff;
  uint32_t rsr_reg_addr = device_oracle_->GetRegAddrInterruptRsr();
  if ((status = Read8(rsr_reg_addr, &rsr)) != ZX_OK) {
    return status;
  }
  rsr &= ~kRsrClearOnReadMask;
  rsr |= kRsrClearOnReadValue;
  if ((status = Write8(rsr_reg_addr, rsr)) != ZX_OK) {
    return status;
  }

  // Configure interrupts to automatically re-enable
  uint8_t misc_cfg;
  uint32_t misc_cfg_reg_addr = device_oracle_->GetRegAddrMiscCfg();
  if ((status = Read8(misc_cfg_reg_addr, &misc_cfg)) != ZX_OK) {
    return status;
  }
  misc_cfg &= ~kMiscCfgAutoReenableMask;
  misc_cfg |= kMiscCfgAutoReenableValue;
  if ((status = Write8(misc_cfg_reg_addr, misc_cfg)) != ZX_OK) {
    return status;
  }

  return status;
}

void BtHciMarvell::OnInitComplete(zx_status_t status, ddk::InitTxn txn) {
  if (status != ZX_OK) {
    zxlogf(ERROR, "Device initialization failed: %s", zx_status_get_string(status));
  }
  txn.Reply(status);
}

// For now, we rely on wlan to load the firmware image for us, we just have to wait for it.
zx_status_t BtHciMarvell::LoadFirmware() {
  uint32_t fw_status_reg_addr = device_oracle_->GetRegAddrFirmwareStatus();

  for (size_t secs_left = 0; secs_left < kFirmwareWaitSeconds; secs_left++) {
    zx_status_t status;
    uint16_t fw_status = 0xffff;

    status = Read16(fw_status_reg_addr, &fw_status);
    if (status == ZX_OK && fw_status == kFirmwareStatusReady) {
      return ZX_OK;
    }
    zx::nanosleep(zx::deadline_after(zx::sec(1)));
  }
  return ZX_ERR_TIMED_OUT;
}

zx_status_t BtHciMarvell::Read8(uint32_t addr, uint8_t* out_value) {
  zx_status_t status = sdio_.DoRwByte(/* write */ false, addr, /* write_byte */ 0, out_value);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failure reading from address %#x: %s", addr, zx_status_get_string(status));
    *out_value = 0xff;
  }
  return status;
}

// Read two adjacent (but not necessarily aligned) registers, and return the value adjusted for
// host endianness.
zx_status_t BtHciMarvell::Read16(uint32_t addr, uint16_t* out_value) {
  zx_status_t status;
  *out_value = 0xffff;
  uint8_t lobits, hibits;

  // Block reads have to be word-aligned, so it's a bit more straightforward to just read the
  // bytes independently.
  if (((status = Read8(addr, &lobits)) == ZX_OK) &&
      ((status = Read8(addr + 1, &hibits)) == ZX_OK)) {
    *out_value = hibits;
    (*out_value) <<= CHAR_BIT;
    *out_value |= lobits;
  }
  return status;
}

// Read three adjacent (but not necessarily aligned) registers, and return the value adjusted for
// host endianness.
zx_status_t BtHciMarvell::Read24(uint32_t addr, uint32_t* out_value) {
  zx_status_t status;
  *out_value = 0xffff;
  uint8_t lobits, midbits, hibits;

  if (((status = Read8(addr, &lobits)) == ZX_OK) &&
      ((status = Read8(addr + 1, &midbits)) == ZX_OK) &&
      ((status = Read8(addr + 2, &hibits)) == ZX_OK)) {
    *out_value = hibits;
    (*out_value) <<= CHAR_BIT;
    *out_value |= midbits;
    (*out_value) <<= CHAR_BIT;
    *out_value |= lobits;
  }
  return status;
}

zx_status_t BtHciMarvell::Write8(uint32_t addr, uint8_t value) {
  zx_status_t status = sdio_.DoRwByte(/* write */ true, addr, value, /* out_read_byte */ nullptr);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failure writing to address %#x: %s", addr, zx_status_get_string(status));
  }
  return status;
}

void BtHciMarvell::DdkUnbind(ddk::UnbindTxn txn) { txn.Reply(); }

void BtHciMarvell::DdkRelease() { delete this; }

zx_status_t BtHciMarvell::DdkGetProtocol(uint32_t proto_id, void* out_proto) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t BtHciMarvell::BtHciOpenCommandChannel(zx::channel channel) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t BtHciMarvell::BtHciOpenAclDataChannel(zx::channel channel) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t BtHciMarvell::BtHciOpenScoChannel(zx::channel channel) { return ZX_ERR_NOT_SUPPORTED; }

zx_status_t BtHciMarvell::BtHciOpenSnoopChannel(zx::channel channel) {
  return ZX_ERR_NOT_SUPPORTED;
}

void BtHciMarvell::BtHciConfigureSco(sco_coding_format_t coding_format, sco_encoding_t encoding,
                                     sco_sample_rate_t sample_rate,
                                     bt_hci_configure_sco_callback callback, void* cookie) {}

void BtHciMarvell::BtHciResetSco(bt_hci_reset_sco_callback callback, void* cookie) {}

}  // namespace bt_hci_marvell

static constexpr zx_driver_ops_t bt_hci_marvell_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = bt_hci_marvell::BtHciMarvell::Bind;
  return ops;
}();

ZIRCON_DRIVER(bt_hci_marvell, bt_hci_marvell_driver_ops, "zircon", "0.1");
