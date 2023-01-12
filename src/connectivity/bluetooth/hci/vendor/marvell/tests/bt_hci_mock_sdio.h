// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_BT_HCI_MOCK_SDIO_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_BT_HCI_MOCK_SDIO_H_

#include <fuchsia/hardware/sdio/cpp/banjo-mock.h>

#include "src/connectivity/bluetooth/hci/vendor/marvell/device_oracle.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

// This function is used by the mock SDIO library to validate calls to DoRwTxn.
bool operator==(const sdio_rw_txn_t& actual, const sdio_rw_txn_t& expected);

namespace bt_hci_marvell {

// An implementation of MockSdio designed for use with testing general lifecycle operation.
class BtHciMockSdio : public ddk::MockSdio {
 public:
  MockSdio& ExpectDoRwTxn(zx_status_t out_status, sdio_rw_txn_t txn) override {
    if (txn.write) {
      block_write_complete_.Reset();
    }
    return ddk::MockSdio::ExpectDoRwTxn(out_status, txn);
  }

  MockSdio& ExpectAckInBandIntr() override {
    ack_complete_.Reset();
    return ddk::MockSdio::ExpectAckInBandIntr();
  }

  // Allow test synchronization with important events.
  libsync::Completion block_write_complete_;
  libsync::Completion ack_complete_;

 private:
  zx_status_t SdioDoRwByte(bool write, uint32_t addr, uint8_t write_byte,
                           uint8_t* out_read_byte) override {
    // If we're performing a write, the out_read_byte parameter is unused. However, ddk::MockSdio
    // will always treat it as an output variable and write to it. So, if we can see that this
    // is an invalid pointer we intercept it and replace it with a valid (but ignored) pointer.
    uint8_t value_read;
    if (write && (out_read_byte == nullptr)) {
      out_read_byte = &value_read;
    }

    // Reads also have an unused parameter, so we want to normalize the value before we pass it to
    // the expectations checker. That way, we're not checking values we really don't care about.
    if (!write) {
      write_byte = 0;
    }

    return ddk::MockSdio::SdioDoRwByte(write, addr, write_byte, out_read_byte);
  }

  zx_status_t SdioDoRwTxn(const sdio_rw_txn_t* txn) override {
    zx_status_t result = ddk::MockSdio::SdioDoRwTxn(txn);
    if (txn->write) {
      block_write_complete_.Signal();
    }
    return result;
  }

  void SdioAckInBandIntr() override {
    ddk::MockSdio::SdioAckInBandIntr();
    ack_complete_.Signal();
  }

  std::optional<DeviceOracle> device_oracle_;
};

}  // namespace bt_hci_marvell

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_BT_HCI_MOCK_SDIO_H_
