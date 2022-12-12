// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_BT_HCI_MOCK_SDIO_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_BT_HCI_MOCK_SDIO_H_

#include <fuchsia/hardware/sdio/cpp/banjo-mock.h>

#include "src/connectivity/bluetooth/hci/vendor/marvell/device-oracle.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

// This function is used by the mock SDIO library to validate calls to DoRwTxn. This is currently
// not expected to be used, so fail if it is.
bool operator==(const sdio_rw_txn_t& actual, const sdio_rw_txn_t& expected) { return false; }

// This function is used by the mock SDIO library to validate calls to DoRwTxnNew. This is currently
// not expected to be used, so fail if it is.
bool operator==(const sdio_rw_txn_new_t& expected, const sdio_rw_txn_new_t& actual) {
  return false;
}

namespace bt_hci_marvell {

// An implementation of MockSdio designed for use with testing general lifecycle operation.
class BtHciMockSdio : public ddk::MockSdio {
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

  std::unique_ptr<DeviceOracle> device_oracle_;
};

}  // namespace bt_hci_marvell

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_BT_HCI_MOCK_SDIO_H_
