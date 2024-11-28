// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fpromise/single_threaded_executor.h>

#include <fbl/unaligned.h>

#include "src/devices/block/drivers/ufs/device_manager.h"
#include "unit-lib.h"

namespace ufs {
using namespace ufs_mock_device;

class InitTest : public UfsTest {
 public:
  void SetUp() override { InitMockDevice(); }
};

TEST_F(InitTest, Basic) { ASSERT_NO_FATAL_FAILURE(StartDriver()); }

TEST_F(InitTest, GetControllerDescriptor) {
  ASSERT_NO_FATAL_FAILURE(StartDriver());

  EXPECT_EQ(dut_->GetDeviceManager().GetDeviceDescriptor().bLength, sizeof(DeviceDescriptor));
  EXPECT_EQ(dut_->GetDeviceManager().GetDeviceDescriptor().bDescriptorIDN,
            static_cast<uint8_t>(DescriptorType::kDevice));
  EXPECT_EQ(dut_->GetDeviceManager().GetDeviceDescriptor().bDeviceSubClass, 0x01);
  EXPECT_EQ(dut_->GetDeviceManager().GetDeviceDescriptor().bNumberWLU, 0x04);
  EXPECT_EQ(dut_->GetDeviceManager().GetDeviceDescriptor().bInitPowerMode, 0x01);
  EXPECT_EQ(dut_->GetDeviceManager().GetDeviceDescriptor().bHighPriorityLUN, 0x7F);
  EXPECT_EQ(dut_->GetDeviceManager().GetDeviceDescriptor().wSpecVersion, htobe16(0x0310));
  EXPECT_EQ(dut_->GetDeviceManager().GetDeviceDescriptor().bUD0BaseOffset, 0x16);
  EXPECT_EQ(dut_->GetDeviceManager().GetDeviceDescriptor().bUDConfigPLength, 0x1A);

  EXPECT_EQ(dut_->GetDeviceManager().GetGeometryDescriptor().bLength, sizeof(GeometryDescriptor));
  EXPECT_EQ(dut_->GetDeviceManager().GetGeometryDescriptor().bDescriptorIDN,
            static_cast<uint8_t>(DescriptorType::kGeometry));
  EXPECT_EQ(fbl::UnalignedLoad<uint64_t>(
                &dut_->GetDeviceManager().GetGeometryDescriptor().qTotalRawDeviceCapacity),
            htobe64(kMockTotalDeviceCapacity >> 9));
  EXPECT_EQ(dut_->GetDeviceManager().GetGeometryDescriptor().bMaxNumberLU, 0x01);
}

TEST_F(InitTest, AddLogicalUnits) {
  constexpr uint8_t kDefualtLunCount = 1;
  constexpr uint8_t kMaxLunCount = 8;

  for (uint8_t lun = kDefualtLunCount; lun < kMaxLunCount; ++lun) {
    mock_device_.AddLun(lun);
  }

  ASSERT_NO_FATAL_FAILURE(StartDriver());
  ASSERT_EQ(dut_->GetLogicalUnitCount(), kMaxLunCount);
}

TEST_F(InitTest, LogicalUnitBlockInfo) {
  ASSERT_NO_FATAL_FAILURE(StartDriver());

  const auto& block_devs = dut_->block_devs();
  scsi::BlockDevice* block_device = block_devs.at(0).at(0).get();

  block_info_t info;
  uint64_t op_size;
  block_device->BlockImplQuery(&info, &op_size);

  ASSERT_EQ(info.block_size, kMockBlockSize);
  ASSERT_EQ(info.block_count, kMockTotalDeviceCapacity / kMockBlockSize);
}

TEST_F(InitTest, UnitAttentionClear) {
  mock_device_.SetUnitAttention(true);
  ASSERT_NO_FATAL_FAILURE(StartDriver());
}

TEST_F(InitTest, Inspect) {
  ASSERT_NO_FATAL_FAILURE(StartDriver());

  fpromise::result<inspect::Hierarchy> hierarchy =
      fpromise::run_single_threaded(inspect::ReadFromInspector(dut_->inspect()));
  ASSERT_TRUE(hierarchy.is_ok());
  const auto* ufs = hierarchy.value().GetByPath({"ufs"});
  ASSERT_NE(ufs, nullptr);

  const auto* version = ufs->GetByPath({"version"});
  ASSERT_NE(version, nullptr);
  auto major_version =
      version->node().get_property<inspect::UintPropertyValue>("major_version_number");
  ASSERT_NE(major_version, nullptr);
  EXPECT_EQ(major_version->value(), kMajorVersion);
  auto minor_version =
      version->node().get_property<inspect::UintPropertyValue>("minor_version_number");
  ASSERT_NE(minor_version, nullptr);
  EXPECT_EQ(minor_version->value(), kMinorVersion);
  auto version_suffix = version->node().get_property<inspect::UintPropertyValue>("version_suffix");
  ASSERT_NE(version_suffix, nullptr);
  EXPECT_EQ(version_suffix->value(), kVersionSuffix);

  const auto* controller = ufs->GetByPath({"controller"});
  ASSERT_NE(controller, nullptr);
  auto logical_unit_count =
      controller->node().get_property<inspect::UintPropertyValue>("logical_unit_count");
  ASSERT_NE(logical_unit_count, nullptr);
  EXPECT_EQ(logical_unit_count->value(), 1U);
  auto reference_clock =
      controller->node().get_property<inspect::StringPropertyValue>("reference_clock");
  ASSERT_NE(reference_clock, nullptr);
  EXPECT_EQ(reference_clock->value(), "19.2 MHz");

  const auto* attributes = controller->GetByPath({"attributes"});
  ASSERT_NE(attributes, nullptr);
  auto boot_lun_enabled = attributes->node().get_property<inspect::UintPropertyValue>("bBootLunEn");
  ASSERT_NE(boot_lun_enabled, nullptr);
  EXPECT_EQ(boot_lun_enabled->value(), 0x01U);

  const auto* unipro = controller->GetByPath({"unipro"});
  ASSERT_NE(unipro, nullptr);
  auto local_version = unipro->node().get_property<inspect::UintPropertyValue>("local_version");
  ASSERT_NE(local_version, nullptr);
  EXPECT_EQ(local_version->value(), kUniproVersion);
}

TEST_F(InitTest, WriteBoosterIsSupportedSharedBuffer) {
  // Shared buffer Type
  mock_device_.GetDeviceDesc().bWriteBoosterBufferType =
      static_cast<uint8_t>(WriteBoosterBufferType::kSharedBuffer);
  mock_device_.GetDeviceDesc().dNumSharedWriteBoosterBufferAllocUnits = betoh32(1);
  ASSERT_NO_FATAL_FAILURE(StartDriver());
  ASSERT_TRUE(dut_->GetDeviceManager().IsWriteBoosterEnabled());
}

TEST_F(InitTest, WriteBoosterIsSupportedDedicatedBuffer) {
  // LU dedicated buffer Type
  mock_device_.GetDeviceDesc().bWriteBoosterBufferType =
      static_cast<uint8_t>(WriteBoosterBufferType::kLuDedicatedBuffer);
  mock_device_.GetLogicalUnit(0).GetUnitDesc().dLUNumWriteBoosterBufferAllocUnits = betoh32(1);
  ASSERT_NO_FATAL_FAILURE(StartDriver());
  ASSERT_TRUE(dut_->GetDeviceManager().IsWriteBoosterEnabled());
}

TEST_F(InitTest, WriteBoosterIsNotSupported) {
  // WriteBooster is not supported.
  ExtendedUfsFeaturesSupport ext_feature_support;
  ext_feature_support.set_writebooster_support(false);
  mock_device_.GetDeviceDesc().dExtendedUfsFeaturesSupport = ext_feature_support.value;
  ASSERT_NO_FATAL_FAILURE(StartDriver());
  ASSERT_FALSE(dut_->GetDeviceManager().IsWriteBoosterEnabled());
}

TEST_F(InitTest, WriteBoosterZeroAllocUnits) {
  // Zero alloc units
  mock_device_.GetDeviceDesc().bWriteBoosterBufferType =
      static_cast<uint8_t>(WriteBoosterBufferType::kLuDedicatedBuffer);
  mock_device_.GetLogicalUnit(0).GetUnitDesc().dLUNumWriteBoosterBufferAllocUnits = betoh32(0);
  ASSERT_NO_FATAL_FAILURE(StartDriver());
  ASSERT_FALSE(dut_->GetDeviceManager().IsWriteBoosterEnabled());
}

TEST_F(InitTest, WriteBoosterBufferLifeTime) {
  // Exceeds buffer life time
  mock_device_.SetAttribute(Attributes::bWBBufferLifeTimeEst, kExceededWriteBoosterBufferLifeTime);
  ASSERT_NO_FATAL_FAILURE(StartDriver());
  ASSERT_FALSE(dut_->GetDeviceManager().IsWriteBoosterEnabled());
}

TEST_F(InitTest, PowerOnWriteProtectEnable) {
  mock_device_.SetFlag(Flags::fPowerOnWPEn, true);
  ASSERT_NO_FATAL_FAILURE(StartDriver());
  ASSERT_TRUE(dut_->GetDeviceManager().IsPowerOnWritePotectEnabled());
}

TEST_F(InitTest, PowerOnWriteProtectDisable) {
  mock_device_.SetFlag(Flags::fPowerOnWPEn, false);
  ASSERT_NO_FATAL_FAILURE(StartDriver());
  ASSERT_FALSE(dut_->GetDeviceManager().IsPowerOnWritePotectEnabled());
}

TEST_F(InitTest, LogicalLunPowerOnWriteProtectEnable) {
  uint8_t lun = 0;
  mock_device_.SetFlag(Flags::fPowerOnWPEn, true);
  mock_device_.GetLogicalUnit(lun).GetUnitDesc().bLUWriteProtect =
      LUWriteProtect::kPowerOnWriteProtect;
  ASSERT_NO_FATAL_FAILURE(StartDriver());
  ASSERT_TRUE(dut_->GetDeviceManager().IsLogicalLunPowerOnWriteProtect());
}

TEST_F(InitTest, LogicalLunPowerOnWriteProtectDisable) {
  uint8_t lun = 0;
  mock_device_.SetFlag(Flags::fPowerOnWPEn, true);
  mock_device_.GetLogicalUnit(lun).GetUnitDesc().bLUWriteProtect = LUWriteProtect::kNoWriteProtect;
  ASSERT_NO_FATAL_FAILURE(StartDriver());
  ASSERT_FALSE(dut_->GetDeviceManager().IsLogicalLunPowerOnWriteProtect());
}

}  // namespace ufs
