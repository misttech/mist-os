// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_

#include <zxtest/zxtest.h>

#include "mock-device/ufs-mock-device.h"
#include "src/devices/block/drivers/ufs/ufs.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace ufs {

class UfsTest : public zxtest::Test {
 public:
  void SetUp() override;

  void RunInit();

  void TearDown() override;

 protected:
  std::shared_ptr<zx_device> fake_root_;
  zx_device* device_;
  std::unique_ptr<ufs_mock_device::UfsMockDevice> mock_device_;
  Ufs* ufs_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_
