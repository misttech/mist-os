// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/hci/vendor/marvell/device_oracle.h"

#include <lib/ddk/debug.h>
#include <zircon/assert.h>

#include <unordered_map>

namespace bt_hci_marvell {

// Product ID (PID) => device
static constexpr struct {
  uint32_t product_id;
  DeviceType device_type;
} kPidLookupTable[] = {{0x914a, DeviceType::k88W8987}};

zx::result<std::unique_ptr<DeviceOracle>> DeviceOracle::Create(uint32_t pid) {
  for (size_t ndx = 0; ndx < std::size(kPidLookupTable); ndx++) {
    if (kPidLookupTable[ndx].product_id == pid) {
      std::unique_ptr<DeviceOracle> result(new DeviceOracle(kPidLookupTable[ndx].device_type));
      return zx::ok(std::move(result));
    }
  }
  return zx::error(ZX_ERR_INVALID_ARGS);
}

uint16_t DeviceOracle::GetSdioBlockSize() const {
  switch (device_type_) {
    case DeviceType::k88W8987:
      return 64;
  }
  ZX_PANIC("Internal error: device type not properly set");
  return 0;
}

uint32_t DeviceOracle::GetRegAddrFirmwareStatus() const {
  switch (device_type_) {
    case DeviceType::k88W8987:
      return 0xe8;
  }
  ZX_PANIC("Internal error: device type not properly set");
  return 0;
}

uint32_t DeviceOracle::GetRegAddrInterruptMask() const {
  switch (device_type_) {
    case DeviceType::k88W8987:
      return 0x08;
  }
  ZX_PANIC("Internal error: device type not properly set");
  return 0;
}

uint32_t DeviceOracle::GetRegAddrInterruptRsr() const {
  switch (device_type_) {
    case DeviceType::k88W8987:
      return 0x04;
  }
  ZX_PANIC("Internal error: device type not properly set");
  return 0;
}

uint32_t DeviceOracle::GetRegAddrInterruptStatus() const {
  switch (device_type_) {
    case DeviceType::k88W8987:
      return 0x0c;
  }
  ZX_PANIC("Internal error: device type not properly set");
  return 0;
}

uint32_t DeviceOracle::GetRegAddrIoportAddr() const {
  switch (device_type_) {
    case DeviceType::k88W8987:
      return 0xe4;
  }
  ZX_PANIC("Internal error: device type not properly set");
  return 0;
}

uint32_t DeviceOracle::GetRegAddrMiscCfg() const {
  switch (device_type_) {
    case DeviceType::k88W8987:
      return 0xd8;
  }
  ZX_PANIC("Internal error: device type not properly set");
  return 0;
}

}  // namespace bt_hci_marvell
