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

TEST_F(InitTest, Inspect) {
  ASSERT_NO_FATAL_FAILURE(RunInit());
  ASSERT_NO_FATAL_FAILURE(ReadInspect(ufs_->inspector().DuplicateVmo()));

  const auto* ufs = hierarchy().GetByPath({"ufs"});
  ASSERT_NOT_NULL(ufs);

  const auto* version = ufs->GetByPath({"version"});
  ASSERT_NOT_NULL(version);
  auto major_version =
      version->node().get_property<inspect::UintPropertyValue>("major_version_number");
  ASSERT_NOT_NULL(major_version);
  EXPECT_EQ(major_version->value(), kMajorVersion);
  auto minor_version =
      version->node().get_property<inspect::UintPropertyValue>("minor_version_number");
  ASSERT_NOT_NULL(minor_version);
  EXPECT_EQ(minor_version->value(), kMinorVersion);
  auto version_suffix = version->node().get_property<inspect::UintPropertyValue>("version_suffix");
  ASSERT_NOT_NULL(version_suffix);
  EXPECT_EQ(version_suffix->value(), kVersionSuffix);

  const auto* controller = ufs->GetByPath({"controller"});
  ASSERT_NOT_NULL(controller);
  auto logical_unit_count =
      controller->node().get_property<inspect::UintPropertyValue>("logical_unit_count");
  ASSERT_NOT_NULL(logical_unit_count);
  EXPECT_EQ(logical_unit_count->value(), 1);
  auto reference_clock =
      controller->node().get_property<inspect::StringPropertyValue>("reference_clock");
  ASSERT_NOT_NULL(reference_clock);
  EXPECT_EQ(reference_clock->value(), "19.2 MHz");

  const auto* attributes = controller->GetByPath({"attributes"});
  ASSERT_NOT_NULL(attributes);
  auto boot_lun_enabled = attributes->node().get_property<inspect::UintPropertyValue>("bBootLunEn");
  ASSERT_NOT_NULL(boot_lun_enabled);
  EXPECT_EQ(boot_lun_enabled->value(), 0x01);

  const auto* unipro = controller->GetByPath({"unipro"});
  ASSERT_NOT_NULL(unipro);
  auto local_version = unipro->node().get_property<inspect::UintPropertyValue>("local_version");
  ASSERT_NOT_NULL(local_version);
  EXPECT_EQ(local_version->value(), kUniproVersion);
}

TEST_F(InitTest, WriteBoosterIsSupportedSharedBuffer) {
  // Shared buffer Type
  mock_device_->GetDeviceDesc().bWriteBoosterBufferType =
      static_cast<uint8_t>(WriteBoosterBufferType::kSharedBuffer);
  mock_device_->GetDeviceDesc().dNumSharedWriteBoosterBufferAllocUnits = betoh32(1);
  ASSERT_NO_FATAL_FAILURE(RunInit());
  ASSERT_TRUE(ufs_->GetDeviceManager().IsWriteBoosterEnabled());
}

TEST_F(InitTest, WriteBoosterIsSupportedDedicatedBuffer) {
  // LU dedicated buffer Type
  mock_device_->GetDeviceDesc().bWriteBoosterBufferType =
      static_cast<uint8_t>(WriteBoosterBufferType::kLuDedicatedBuffer);
  mock_device_->GetLogicalUnit(0).GetUnitDesc().dLUNumWriteBoosterBufferAllocUnits = betoh32(1);
  ASSERT_NO_FATAL_FAILURE(RunInit());
  ASSERT_TRUE(ufs_->GetDeviceManager().IsWriteBoosterEnabled());
}

TEST_F(InitTest, WriteBoosterIsNotSupported) {
  // WriteBooster is not supported.
  ExtendedUfsFeaturesSupport ext_feature_support;
  ext_feature_support.set_writebooster_support(false);
  mock_device_->GetDeviceDesc().dExtendedUfsFeaturesSupport = ext_feature_support.value;
  ASSERT_NO_FATAL_FAILURE(RunInit());
  ASSERT_FALSE(ufs_->GetDeviceManager().IsWriteBoosterEnabled());
}

TEST_F(InitTest, WriteBoosterZeroAllocUnits) {
  // Zero alloc units
  mock_device_->GetDeviceDesc().bWriteBoosterBufferType =
      static_cast<uint8_t>(WriteBoosterBufferType::kLuDedicatedBuffer);
  mock_device_->GetLogicalUnit(0).GetUnitDesc().dLUNumWriteBoosterBufferAllocUnits = betoh32(0);
  ASSERT_NO_FATAL_FAILURE(RunInit());
  ASSERT_FALSE(ufs_->GetDeviceManager().IsWriteBoosterEnabled());
}

TEST_F(InitTest, WriteBoosterBufferLifeTime) {
  // Exceeds buffer life time
  mock_device_->SetAttribute(Attributes::bWBBufferLifeTimeEst, kExceededWriteBoosterBufferLifeTime);
  ASSERT_NO_FATAL_FAILURE(RunInit());
  ASSERT_FALSE(ufs_->GetDeviceManager().IsWriteBoosterEnabled());
}

}  // namespace ufs
