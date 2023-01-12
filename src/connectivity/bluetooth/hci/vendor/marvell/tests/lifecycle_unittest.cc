// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/hci/vendor/marvell/tests/bt_hci_mock_sdio_test.h"

namespace bt_hci_marvell {

// Verify that we can successfully bind, initialize, unbind, and release the driver.
TEST_F(BtHciMockSdioTest, LifecycleTest) {}

TEST_F(BtHciMockSdioTest, GetProtocols) {
  bt_hci_protocol_t bt_hci_proto;
  EXPECT_OK(driver_instance_->DdkGetProtocol(ZX_PROTOCOL_BT_HCI, &bt_hci_proto));

  sdio_protocol_t sdio_proto;
  EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, driver_instance_->DdkGetProtocol(ZX_PROTOCOL_SDIO, &sdio_proto));
}

}  // namespace bt_hci_marvell
