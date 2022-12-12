// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/hci/vendor/marvell/device-oracle.h"

#include <zxtest/zxtest.h>

namespace bt_hci_marvell {

class DeviceOracleTest : public zxtest::Test {};

// Verify that an attempt to create an oracle with an invalid product ID fails
TEST_F(DeviceOracleTest, BadDeviceType) {
  uint32_t pid = 0x1234;
  zx::result<std::unique_ptr<DeviceOracle>> result = DeviceOracle::Create(pid);
  ASSERT_TRUE(result.is_error());
  EXPECT_EQ(result.error_value(), ZX_ERR_INVALID_ARGS);
}

// Verify the results of all accessors for an 88W8987 device
TEST_F(DeviceOracleTest, DeviceType88W8987) {
  uint32_t pid = 0x914a;
  zx::result<std::unique_ptr<DeviceOracle>> result = DeviceOracle::Create(pid);
  ASSERT_TRUE(result.is_ok());

  std::unique_ptr<DeviceOracle> oracle = std::move(result.value());
  EXPECT_EQ(oracle->GetSdioBlockSize(), 64);
  EXPECT_EQ(oracle->GetRegAddrFirmwareStatus(), 0xe8);
  EXPECT_EQ(oracle->GetRegAddrInterruptRsr(), 0x04);
  EXPECT_EQ(oracle->GetRegAddrIoportAddr(), 0xe4);
  EXPECT_EQ(oracle->GetRegAddrMiscCfg(), 0xd8);
}

}  // namespace bt_hci_marvell
