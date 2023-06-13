// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "unit-lib.h"

namespace ufs {
using namespace ufs_mock_device;

using InitTest = UfsTest;

namespace {
inline uint64_t UnalignedLoad64(const uint64_t* ptr) {
  uint64_t value;
  memcpy(&value, ptr, sizeof(uint64_t));
  return value;
}
}  // namespace

TEST_F(InitTest, Basic) { ASSERT_NO_FATAL_FAILURE(RunInit()); }

TEST_F(InitTest, GetControllerDescriptor) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  EXPECT_EQ(ufs_->GetDeviceDescriptor().bLength, sizeof(DeviceDescriptor));
  EXPECT_EQ(ufs_->GetDeviceDescriptor().bDescriptorIDN,
            static_cast<uint8_t>(DescriptorType::kDevice));
  EXPECT_EQ(ufs_->GetDeviceDescriptor().bDeviceSubClass, 0x01);
  EXPECT_EQ(ufs_->GetDeviceDescriptor().bNumberWLU, 0x04);
  EXPECT_EQ(ufs_->GetDeviceDescriptor().bInitPowerMode, 0x01);
  EXPECT_EQ(ufs_->GetDeviceDescriptor().bHighPriorityLUN, 0x7F);
  EXPECT_EQ(ufs_->GetDeviceDescriptor().wSpecVersion, htobe16(0x0310));
  EXPECT_EQ(ufs_->GetDeviceDescriptor().bUD0BaseOffset, 0x16);
  EXPECT_EQ(ufs_->GetDeviceDescriptor().bUDConfigPLength, 0x1A);

  EXPECT_EQ(ufs_->GetGeometryDescriptor().bLength, sizeof(GeometryDescriptor));
  EXPECT_EQ(ufs_->GetGeometryDescriptor().bDescriptorIDN,
            static_cast<uint8_t>(DescriptorType::kGeometry));
  EXPECT_EQ(UnalignedLoad64(&ufs_->GetGeometryDescriptor().qTotalRawDeviceCapacity),
            htobe64(kMockTotalDeviceCapacity >> 9));
  EXPECT_EQ(ufs_->GetGeometryDescriptor().bMaxNumberLU, 0x01);
}

TEST_F(InitTest, ScanLogicalUnits) {
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

}  // namespace ufs
