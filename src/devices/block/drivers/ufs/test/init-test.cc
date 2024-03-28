// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fbl/unaligned.h>

#include "src/devices/block/drivers/ufs/device_manager.h"
#include "unit-lib.h"

namespace ufs {
using namespace ufs_mock_device;

using InitTest = UfsTest;

TEST_F(InitTest, Basic) { ASSERT_NO_FATAL_FAILURE(RunInit()); }

TEST_F(InitTest, GetControllerDescriptor) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  EXPECT_EQ(ufs_->GetDeviceManager().GetDeviceDescriptor().bLength, sizeof(DeviceDescriptor));
  EXPECT_EQ(ufs_->GetDeviceManager().GetDeviceDescriptor().bDescriptorIDN,
            static_cast<uint8_t>(DescriptorType::kDevice));
  EXPECT_EQ(ufs_->GetDeviceManager().GetDeviceDescriptor().bDeviceSubClass, 0x01);
  EXPECT_EQ(ufs_->GetDeviceManager().GetDeviceDescriptor().bNumberWLU, 0x04);
  EXPECT_EQ(ufs_->GetDeviceManager().GetDeviceDescriptor().bInitPowerMode, 0x01);
  EXPECT_EQ(ufs_->GetDeviceManager().GetDeviceDescriptor().bHighPriorityLUN, 0x7F);
  EXPECT_EQ(ufs_->GetDeviceManager().GetDeviceDescriptor().wSpecVersion, htobe16(0x0310));
  EXPECT_EQ(ufs_->GetDeviceManager().GetDeviceDescriptor().bUD0BaseOffset, 0x16);
  EXPECT_EQ(ufs_->GetDeviceManager().GetDeviceDescriptor().bUDConfigPLength, 0x1A);

  EXPECT_EQ(ufs_->GetDeviceManager().GetGeometryDescriptor().bLength, sizeof(GeometryDescriptor));
  EXPECT_EQ(ufs_->GetDeviceManager().GetGeometryDescriptor().bDescriptorIDN,
            static_cast<uint8_t>(DescriptorType::kGeometry));
  EXPECT_EQ(fbl::UnalignedLoad<uint64_t>(
                &ufs_->GetDeviceManager().GetGeometryDescriptor().qTotalRawDeviceCapacity),
            htobe64(kMockTotalDeviceCapacity >> 9));
  EXPECT_EQ(ufs_->GetDeviceManager().GetGeometryDescriptor().bMaxNumberLU, 0x01);
}

TEST_F(InitTest, AddLogicalUnits) {
  constexpr uint8_t kDefualtLunCount = 1;
  constexpr uint8_t kMaxLunCount = 8;

  for (uint8_t lun = kDefualtLunCount; lun < kMaxLunCount; ++lun) {
    mock_device_->AddLun(lun);
  }

  ASSERT_NO_FATAL_FAILURE(RunInit());
  ASSERT_EQ(ufs_->GetLogicalUnitCount(), kMaxLunCount);
}

TEST_F(InitTest, LogicalUnitBlockInfo) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  zx_device* logical_unit_device = device_->GetLatestChild();
  ddk::BlockImplProtocolClient client(logical_unit_device);
  block_info_t info;
  uint64_t op_size;
  client.Query(&info, &op_size);

  ASSERT_EQ(info.block_size, kMockBlockSize);
  ASSERT_EQ(info.block_count, kMockTotalDeviceCapacity / kMockBlockSize);
}

TEST_F(InitTest, UnitAttentionClear) {
  mock_device_->SetUnitAttention(true);
  ASSERT_NO_FATAL_FAILURE(RunInit());
}

TEST_F(InitTest, SuspendAndResume) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  ASSERT_FALSE(ufs_->IsSuspended());
  UfsPowerMode power_mode = UfsPowerMode::kActive;
  ASSERT_EQ(ufs_->GetDeviceManager().GetCurrentPowerMode(), power_mode);
  ASSERT_EQ(ufs_->GetDeviceManager().GetCurrentPowerCondition(),
            ufs_->GetDeviceManager().GetPowerModeMap()[power_mode].first);
  ASSERT_EQ(ufs_->GetDeviceManager().GetCurrentLinkState(),
            ufs_->GetDeviceManager().GetPowerModeMap()[power_mode].second);

  // Issue DdkSuspend and verify that errors are returned for subsequent operations.
  ddk::SuspendTxn suspend_txn(device_, 0, false, DEVICE_SUSPEND_REASON_REBOOT);
  ufs_->DdkSuspend(std::move(suspend_txn));
  fake_root_->GetLatestChild()->WaitUntilSuspendReplyCalled();
  ASSERT_TRUE(ufs_->IsSuspended());
  power_mode = UfsPowerMode::kSleep;
  ASSERT_EQ(ufs_->GetDeviceManager().GetCurrentPowerMode(), power_mode);
  ASSERT_EQ(ufs_->GetDeviceManager().GetCurrentPowerCondition(),
            ufs_->GetDeviceManager().GetPowerModeMap()[power_mode].first);
  ASSERT_EQ(ufs_->GetDeviceManager().GetCurrentLinkState(),
            ufs_->GetDeviceManager().GetPowerModeMap()[power_mode].second);

  ddk::ResumeTxn resume_txn(device_, 0);
  ufs_->DdkResume(std::move(resume_txn));
  fake_root_->GetLatestChild()->WaitUntilResumeReplyCalled();
  ASSERT_FALSE(ufs_->IsSuspended());
  power_mode = UfsPowerMode::kActive;
  ASSERT_EQ(ufs_->GetDeviceManager().GetCurrentPowerMode(), power_mode);
  ASSERT_EQ(ufs_->GetDeviceManager().GetCurrentPowerCondition(),
            ufs_->GetDeviceManager().GetPowerModeMap()[power_mode].first);
  ASSERT_EQ(ufs_->GetDeviceManager().GetCurrentLinkState(),
            ufs_->GetDeviceManager().GetPowerModeMap()[power_mode].second);
}

}  // namespace ufs
