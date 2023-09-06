// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cstdint>
#include <memory>

#include "src/devices/block/drivers/ufs/transfer_request_descriptor.h"
#include "src/devices/block/drivers/ufs/upiu/attributes.h"
#include "src/devices/block/drivers/ufs/upiu/descriptors.h"
#include "src/devices/block/drivers/ufs/upiu/upiu_transactions.h"
#include "unit-lib.h"

namespace ufs {
using namespace ufs_mock_device;

using QueryRequestTest = UfsTest;

TEST_F(QueryRequestTest, DeviceDescriptor) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  ReadDescriptorUpiu device_desc_upiu(DescriptorType::kDevice);
  auto response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          device_desc_upiu);
  ASSERT_OK(response);
  auto device_descriptor =
      response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<DeviceDescriptor>();

  ASSERT_EQ(device_descriptor.bLength, mock_device_->GetDeviceDesc().bLength);
  ASSERT_EQ(device_descriptor.bDescriptorIDN, mock_device_->GetDeviceDesc().bDescriptorIDN);
  ASSERT_EQ(device_descriptor.bDeviceSubClass, mock_device_->GetDeviceDesc().bDeviceSubClass);
  ASSERT_EQ(device_descriptor.bNumberWLU, mock_device_->GetDeviceDesc().bNumberWLU);
  ASSERT_EQ(device_descriptor.bInitPowerMode, mock_device_->GetDeviceDesc().bInitPowerMode);
  ASSERT_EQ(device_descriptor.bHighPriorityLUN, mock_device_->GetDeviceDesc().bHighPriorityLUN);
  ASSERT_EQ(device_descriptor.wSpecVersion, mock_device_->GetDeviceDesc().wSpecVersion);
  ASSERT_EQ(device_descriptor.bUD0BaseOffset, mock_device_->GetDeviceDesc().bUD0BaseOffset);
  ASSERT_EQ(device_descriptor.bUDConfigPLength, mock_device_->GetDeviceDesc().bUDConfigPLength);
}

TEST_F(QueryRequestTest, GeometryDescriptor) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  ReadDescriptorUpiu geometry_desc_upiu(DescriptorType::kGeometry);
  auto response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          geometry_desc_upiu);
  ASSERT_OK(response);
  auto geometry_desc =
      response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<GeometryDescriptor>();

  ASSERT_EQ(geometry_desc.bLength, mock_device_->GetGeometryDesc().bLength);
  ASSERT_EQ(geometry_desc.bDescriptorIDN, mock_device_->GetGeometryDesc().bDescriptorIDN);
  ASSERT_EQ(geometry_desc.bMaxNumberLU, mock_device_->GetGeometryDesc().bMaxNumberLU);
}

TEST_F(QueryRequestTest, UnitDescriptor) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  uint8_t lun = 0;
  ReadDescriptorUpiu unit_desc_upiu(DescriptorType::kUnit, lun);
  auto response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          unit_desc_upiu);
  ASSERT_OK(response);
  auto unit_desc = response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<UnitDescriptor>();

  const auto& mock_desc = mock_device_->GetLogicalUnit(lun).GetUnitDesc();
  ASSERT_EQ(unit_desc.bLength, mock_desc.bLength);
  ASSERT_EQ(unit_desc.bDescriptorIDN, mock_desc.bDescriptorIDN);
  ASSERT_EQ(unit_desc.bLUEnable, mock_desc.bLUEnable);
  ASSERT_EQ(unit_desc.bLogicalBlockSize, mock_desc.bLogicalBlockSize);
}

TEST_F(QueryRequestTest, WriteDescriptor) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  // 0x7F: Same priority for all LUNs
  // If all LUNs already have the same priority, change the priority of LUN0 higher
  // If specific LUN has higher priority, change to the same priority for all LUNs
  uint8_t high_priority_lun = mock_device_->GetDeviceDesc().bHighPriorityLUN == 0x7F ? 0 : 0x7F;

  ConfigurationDescriptor descriptor;
  std::memset(&descriptor, 0, sizeof(ConfigurationDescriptor));
  descriptor.bLength = 0xE6;
  descriptor.bDescriptorIDN = 0x01;
  descriptor.bConfDescContinue = 0x00;
  descriptor.bHighPriorityLUN = high_priority_lun;

  WriteDescriptorUpiu config_desc_upiu(DescriptorType::kConfiguration, &descriptor);
  auto response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          config_desc_upiu);
  ASSERT_OK(response);

  ASSERT_EQ(high_priority_lun, mock_device_->GetDeviceDesc().bHighPriorityLUN);
}

TEST_F(QueryRequestTest, WriteAttribute) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  mock_device_->SetAttribute(Attributes::bCurrentPowerMode, 0);

  uint8_t power_mode = 0x11;  // Active power mode
  WriteAttributeUpiu write_attribute_upiu(Attributes::bCurrentPowerMode, power_mode);
  auto response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          write_attribute_upiu);
  ASSERT_OK(response);
  ASSERT_EQ(power_mode, mock_device_->GetAttribute(Attributes::bCurrentPowerMode));
}

TEST_F(QueryRequestTest, ReadAttribute) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  uint8_t power_mode = 0x11;  // Active power mode
  mock_device_->SetAttribute(Attributes::bCurrentPowerMode, power_mode);

  ReadAttributeUpiu read_attribute_upiu(Attributes::bCurrentPowerMode);
  auto response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          read_attribute_upiu);
  ASSERT_OK(response);
  auto attribute = response->GetResponse<AttributeResponseUpiu>().GetAttribute();

  ASSERT_EQ(attribute, power_mode);
}

TEST_F(QueryRequestTest, ReadFlag) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  bool device_init = false;
  mock_device_->SetFlag(Flags::fDeviceInit, device_init);

  ReadFlagUpiu read_flag_upiu(Flags::fDeviceInit);
  auto response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          read_flag_upiu);
  ASSERT_OK(response);
  auto flag = response->GetResponse<FlagResponseUpiu>().GetFlag();

  ASSERT_EQ(flag, device_init);
}

TEST_F(QueryRequestTest, SetFlag) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  mock_device_->SetFlag(Flags::fPermanentWPEn, false);

  SetFlagUpiu set_flag_upiu(Flags::fPermanentWPEn);
  auto response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          set_flag_upiu);
  ASSERT_OK(response);

  ASSERT_EQ(true, mock_device_->GetFlag(Flags::fPermanentWPEn));
}

TEST_F(QueryRequestTest, ToggleFlag) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  bool device_init = false;
  mock_device_->SetFlag(Flags::fDeviceInit, device_init);

  ToggleFlagUpiu toggle_flag_upiu(Flags::fDeviceInit);
  auto response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          toggle_flag_upiu);
  ASSERT_OK(response);

  ASSERT_EQ(!device_init, mock_device_->GetFlag(Flags::fDeviceInit));
}

TEST_F(QueryRequestTest, ClearFlag) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  bool device_init = true;
  mock_device_->SetFlag(Flags::fDeviceInit, device_init);

  ClearFlagUpiu clear_flag_upiu(Flags::fDeviceInit);
  auto response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          clear_flag_upiu);
  ASSERT_OK(response);

  device_init = false;
  ASSERT_EQ(device_init, mock_device_->GetFlag(Flags::fDeviceInit));
}

}  // namespace ufs
