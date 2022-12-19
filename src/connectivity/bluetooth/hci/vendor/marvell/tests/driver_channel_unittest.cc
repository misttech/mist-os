// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/hci/vendor/marvell/tests/bt_hci_mock_sdio_test.h"

namespace bt_hci_marvell {

// Verify that we can open host channels for communicating with the driver, and that attempts to
// create more than one channel of a given type fail.
TEST_F(BtHciMockSdioTest, SimpleChannelOpenTest) {
  zx::channel cmd_ch, cmd_ch2;
  EXPECT_OK(driver_instance_->BtHciOpenCommandChannel(std::move(cmd_ch)));
  EXPECT_NOT_OK(driver_instance_->BtHciOpenCommandChannel(std::move(cmd_ch2)));

  zx::channel acl_data_ch, acl_data_ch2;
  EXPECT_OK(driver_instance_->BtHciOpenAclDataChannel(std::move(acl_data_ch)));
  EXPECT_NOT_OK(driver_instance_->BtHciOpenAclDataChannel(std::move(acl_data_ch2)));

  zx::channel sco_data_ch, sco_data_ch2;
  EXPECT_OK(driver_instance_->BtHciOpenScoChannel(std::move(sco_data_ch)));
  EXPECT_NOT_OK(driver_instance_->BtHciOpenScoChannel(std::move(sco_data_ch2)));
}

}  // namespace bt_hci_marvell
