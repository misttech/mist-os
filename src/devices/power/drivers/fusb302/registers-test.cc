// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/fusb302/registers.h"

#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/mock-i2c/mock-i2c.h>
#include <lib/zx/result.h>

#include <utility>

#include <zxtest/zxtest.h>

namespace fusb302 {

namespace {

TEST(DeviceIdRegTest, VersionCharacter) {
  // Test cases from page 19 in the data sheet.

  auto f302a_device_id = DeviceIdReg::Get().FromValue(0b1000'00'00);
  EXPECT_EQ('A', f302a_device_id.VersionCharacter());

  auto f302b_device_id = DeviceIdReg::Get().FromValue(0b1001'00'00);
  EXPECT_EQ('B', f302b_device_id.VersionCharacter());

  auto f302c_device_id = DeviceIdReg::Get().FromValue(0b1010'00'00);
  EXPECT_EQ('C', f302c_device_id.VersionCharacter());
}

TEST(DeviceIdRegTest, RevisionCharacter) {
  // Test cases from page 19 in the data sheet.

  auto f302b_reva_device_id = DeviceIdReg::Get().FromValue(0b1001'00'00);
  EXPECT_EQ('A', f302b_reva_device_id.RevisionCharacter());

  auto f302b_revb_device_id = DeviceIdReg::Get().FromValue(0b1001'00'01);
  EXPECT_EQ('B', f302b_revb_device_id.RevisionCharacter());

  auto f302b_revc_device_id = DeviceIdReg::Get().FromValue(0b1001'00'10);
  EXPECT_EQ('C', f302b_revc_device_id.RevisionCharacter());

  auto f302b_revd_device_id = DeviceIdReg::Get().FromValue(0b1001'00'11);
  EXPECT_EQ('D', f302b_revd_device_id.RevisionCharacter());
}

TEST(DeviceIdRegTest, ProductString) {
  // Test cases from page 19 in the data sheet.

  auto f302bmpx_device_id = DeviceIdReg::Get().FromValue(0b1001'00'00);
  EXPECT_STREQ("FUSB302BMPX", f302bmpx_device_id.ProductString());

  auto f302b01mpx_device_id = DeviceIdReg::Get().FromValue(0b1001'01'00);
  EXPECT_STREQ("FUSB302B01MPX", f302b01mpx_device_id.ProductString());
}

TEST(Switches0RegTest, SwitchBlockConfigForCC1) {
  auto switches0 = Switches0Reg::Get().FromValue(0);

  switches0.set_reg_value(0).SetSwitchBlockConfig(usb_pd::ConfigChannelPinId::kCc1,
                                                  SwitchBlockConfig::kOpen);
  EXPECT_EQ(false, switches0.pdwn1());
  EXPECT_EQ(false, switches0.vconn_cc1());
  EXPECT_EQ(false, switches0.pu_en1());
  EXPECT_EQ(SwitchBlockConfig::kOpen,
            switches0.SwitchBlockConfigFor(usb_pd::ConfigChannelPinId::kCc1));

  switches0.set_reg_value(0).SetSwitchBlockConfig(usb_pd::ConfigChannelPinId::kCc1,
                                                  SwitchBlockConfig::kPullDown);
  EXPECT_EQ(true, switches0.pdwn1());
  EXPECT_EQ(false, switches0.vconn_cc1());
  EXPECT_EQ(false, switches0.pu_en1());
  EXPECT_EQ(SwitchBlockConfig::kPullDown,
            switches0.SwitchBlockConfigFor(usb_pd::ConfigChannelPinId::kCc1));

  switches0.set_reg_value(0).SetSwitchBlockConfig(usb_pd::ConfigChannelPinId::kCc1,
                                                  SwitchBlockConfig::kConnectorVoltage);
  EXPECT_EQ(false, switches0.pdwn1());
  EXPECT_EQ(true, switches0.vconn_cc1());
  EXPECT_EQ(false, switches0.pu_en1());
  EXPECT_EQ(SwitchBlockConfig::kConnectorVoltage,
            switches0.SwitchBlockConfigFor(usb_pd::ConfigChannelPinId::kCc1));

  switches0.set_reg_value(0).SetSwitchBlockConfig(usb_pd::ConfigChannelPinId::kCc1,
                                                  SwitchBlockConfig::kPullUp);
  EXPECT_EQ(false, switches0.pdwn1());
  EXPECT_EQ(false, switches0.vconn_cc1());
  EXPECT_EQ(true, switches0.pu_en1());
  EXPECT_EQ(SwitchBlockConfig::kPullUp,
            switches0.SwitchBlockConfigFor(usb_pd::ConfigChannelPinId::kCc1));
}

TEST(Switches0RegTest, SwitchBlockConfigForCC2) {
  auto switches0 = Switches0Reg::Get().FromValue(0);

  switches0.set_reg_value(0).SetSwitchBlockConfig(usb_pd::ConfigChannelPinId::kCc2,
                                                  SwitchBlockConfig::kOpen);
  EXPECT_EQ(false, switches0.pdwn2());
  EXPECT_EQ(false, switches0.vconn_cc2());
  EXPECT_EQ(false, switches0.pu_en2());
  EXPECT_EQ(SwitchBlockConfig::kOpen,
            switches0.SwitchBlockConfigFor(usb_pd::ConfigChannelPinId::kCc2));

  switches0.set_reg_value(0).SetSwitchBlockConfig(usb_pd::ConfigChannelPinId::kCc2,
                                                  SwitchBlockConfig::kPullDown);
  EXPECT_EQ(true, switches0.pdwn2());
  EXPECT_EQ(false, switches0.vconn_cc2());
  EXPECT_EQ(false, switches0.pu_en2());
  EXPECT_EQ(SwitchBlockConfig::kPullDown,
            switches0.SwitchBlockConfigFor(usb_pd::ConfigChannelPinId::kCc2));

  switches0.set_reg_value(0).SetSwitchBlockConfig(usb_pd::ConfigChannelPinId::kCc2,
                                                  SwitchBlockConfig::kConnectorVoltage);
  EXPECT_EQ(false, switches0.pdwn2());
  EXPECT_EQ(true, switches0.vconn_cc2());
  EXPECT_EQ(false, switches0.pu_en2());
  EXPECT_EQ(SwitchBlockConfig::kConnectorVoltage,
            switches0.SwitchBlockConfigFor(usb_pd::ConfigChannelPinId::kCc2));

  switches0.set_reg_value(0).SetSwitchBlockConfig(usb_pd::ConfigChannelPinId::kCc2,
                                                  SwitchBlockConfig::kPullUp);
  EXPECT_EQ(false, switches0.pdwn2());
  EXPECT_EQ(false, switches0.vconn_cc2());
  EXPECT_EQ(true, switches0.pu_en2());
  EXPECT_EQ(SwitchBlockConfig::kPullUp,
            switches0.SwitchBlockConfigFor(usb_pd::ConfigChannelPinId::kCc2));
}

TEST(Switches0RegTest, MeasureBlockInput) {
  auto switches0 = Switches0Reg::Get().FromValue(0);

  switches0.set_reg_value(0).SetMeasureBlockInput(usb_pd::ConfigChannelPinSwitch::kNone);
  EXPECT_EQ(false, switches0.meas_cc1());
  EXPECT_EQ(false, switches0.meas_cc2());
  EXPECT_EQ(usb_pd::ConfigChannelPinSwitch::kNone, switches0.MeasureBlockInput());

  switches0.set_reg_value(0).SetMeasureBlockInput(usb_pd::ConfigChannelPinSwitch::kCc1);
  EXPECT_EQ(true, switches0.meas_cc1());
  EXPECT_EQ(false, switches0.meas_cc2());
  EXPECT_EQ(usb_pd::ConfigChannelPinSwitch::kCc1, switches0.MeasureBlockInput());

  switches0.set_reg_value(0).SetMeasureBlockInput(usb_pd::ConfigChannelPinSwitch::kCc2);
  EXPECT_EQ(false, switches0.meas_cc1());
  EXPECT_EQ(true, switches0.meas_cc2());
  EXPECT_EQ(usb_pd::ConfigChannelPinSwitch::kCc2, switches0.MeasureBlockInput());
}

TEST(Switches1RegTest, BmcPhyConnection) {
  auto switches1 = Switches1Reg::Get().FromValue(0);

  switches1.set_reg_value(0).SetBmcPhyConnection(usb_pd::ConfigChannelPinSwitch::kNone);
  EXPECT_EQ(false, switches1.txcc1());
  EXPECT_EQ(false, switches1.txcc2());
  EXPECT_EQ(usb_pd::ConfigChannelPinSwitch::kNone, switches1.BmcPhyConnection());

  switches1.set_reg_value(0).SetBmcPhyConnection(usb_pd::ConfigChannelPinSwitch::kCc1);
  EXPECT_EQ(true, switches1.txcc1());
  EXPECT_EQ(false, switches1.txcc2());
  EXPECT_EQ(usb_pd::ConfigChannelPinSwitch::kCc1, switches1.BmcPhyConnection());

  switches1.set_reg_value(0).SetBmcPhyConnection(usb_pd::ConfigChannelPinSwitch::kCc2);
  EXPECT_EQ(false, switches1.txcc1());
  EXPECT_EQ(true, switches1.txcc2());
  EXPECT_EQ(usb_pd::ConfigChannelPinSwitch::kCc2, switches1.BmcPhyConnection());
}

class Fusb302RegisterTest : public zxtest::Test {
 public:
  void SetUp() override {
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_i2c::Device>();
    EXPECT_TRUE(endpoints.is_ok());
    mock_i2c_client_ = std::move(endpoints->client);

    EXPECT_OK(loop_.StartThread());
    fidl::BindServer<fuchsia_hardware_i2c::Device>(loop_.dispatcher(), std::move(endpoints->server),
                                                   &mock_i2c_);
  }

 protected:
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  mock_i2c::MockI2c mock_i2c_;
  fidl::ClientEnd<fuchsia_hardware_i2c::Device> mock_i2c_client_;
};

class ReadModifyWriteTestReg : public Fusb302Register<ReadModifyWriteTestReg> {
 public:
  DEF_FIELD(3, 0, documented_field);

  static auto Get() { return hwreg::I2cRegisterAddr<ReadModifyWriteTestReg>(0x42); }
};

TEST_F(Fusb302RegisterTest, ReadModifyWriteReportsOldValue) {
  mock_i2c_.ExpectWrite({0x42}).ExpectReadStop({0xf5});

  bool mutator_called = false;
  uint8_t old_documented_field_value = 0xff;
  zx::result<> read_modify_write_result = ReadModifyWriteTestReg::ReadModifyWrite(
      mock_i2c_client_, [&](ReadModifyWriteTestReg& test_register) {
        mutator_called = true;
        old_documented_field_value = test_register.documented_field();
      });
  EXPECT_OK(read_modify_write_result);
  EXPECT_TRUE(mutator_called);
  EXPECT_EQ(0x05, old_documented_field_value);
}

TEST_F(Fusb302RegisterTest, ReadModifyWriteNoChange) {
  mock_i2c_.ExpectWrite({0x42}).ExpectReadStop({0xf5});

  bool mutator_called = false;
  zx::result<> read_modify_write_result = ReadModifyWriteTestReg::ReadModifyWrite(
      mock_i2c_client_, [&](ReadModifyWriteTestReg& test_register) { mutator_called = true; });
  EXPECT_OK(read_modify_write_result);
  EXPECT_TRUE(mutator_called);
}

TEST_F(Fusb302RegisterTest, ReadModifyWriteChange) {
  mock_i2c_.ExpectWrite({0x42}).ExpectReadStop({0xf5});
  mock_i2c_.ExpectWriteStop({0x42, 0xfc});

  bool mutator_called = false;
  zx::result<> read_modify_write_result = ReadModifyWriteTestReg::ReadModifyWrite(
      mock_i2c_client_, [&](ReadModifyWriteTestReg& test_register) {
        mutator_called = true;
        test_register.set_documented_field(0x0c);
      });
  EXPECT_OK(read_modify_write_result);
  EXPECT_TRUE(mutator_called);
}

}  // namespace

}  // namespace fusb302
