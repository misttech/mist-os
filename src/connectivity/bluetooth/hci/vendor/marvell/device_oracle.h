// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_DEVICE_ORACLE_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_DEVICE_ORACLE_H_

#include <lib/zx/result.h>
#include <stdint.h>

#include <memory>

namespace bt_hci_marvell {

enum class DeviceType {
  k88W8987,
};

// Value that indicates that firmware has been successfully loaded
constexpr uint16_t kFirmwareStatusReady = 0xfedc;

// Value to set in RSR register for interrupts to clear-on-read
constexpr uint8_t kRsrClearOnReadMask = 0x3f;
constexpr uint8_t kRsrClearOnReadValue = 0x3f;

// Value to set in MiscCfg register for interrupts to automatically re-enable
constexpr uint8_t kMiscCfgAutoReenableMask = 0x10;
constexpr uint8_t kMiscCfgAutoReenableValue = 0x10;

// These are the bits in the interrupt mask register that we use
constexpr uint8_t kInterruptMaskAllBits = 0x03;
// Interrupt we receive when the card is ready to process another packet
constexpr uint8_t kInterruptMaskReadyToSend = 0x02;
// Interrupt we receive when the card is sending us a packet
constexpr uint8_t kInterruptMaskPacketAvailable = 0x01;

// A DeviceOracle provides functions to retrieve values that are specific to a product.
class DeviceOracle {
 public:
  // Factory method
  static zx::result<std::unique_ptr<DeviceOracle>> Create(uint32_t pid);

  DeviceOracle() = delete;

  uint16_t GetSdioBlockSize() const;
  uint32_t GetRegAddrFirmwareStatus() const;
  uint32_t GetRegAddrInterruptMask() const;
  uint32_t GetRegAddrInterruptRsr() const;
  uint32_t GetRegAddrInterruptStatus() const;
  uint32_t GetRegAddrIoportAddr() const;
  uint32_t GetRegAddrMiscCfg() const;

 private:
  explicit DeviceOracle(DeviceType device_type) : device_type_(device_type) {}

  DeviceType device_type_;
};

}  // namespace bt_hci_marvell

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_DEVICE_ORACLE_H_
