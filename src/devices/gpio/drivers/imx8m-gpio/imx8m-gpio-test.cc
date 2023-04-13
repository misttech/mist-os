// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "imx8m-gpio.h"

#include <lib/mock-function/mock-function.h>

#include <fbl/algorithm.h>
#include <mock-mmio-reg/mock-mmio-reg.h>
#include <zxtest/zxtest.h>

namespace {

constexpr imx8m::PinConfigMetadata kPinConfigMetadata{[]() {
  imx8m::PinConfigMetadata pinconfig_metadata{};

  pinconfig_metadata.port_info[0].pin_count = 30;
  pinconfig_metadata.port_info[1].pin_count = 21;
  pinconfig_metadata.port_info[2].pin_count = 26;
  pinconfig_metadata.port_info[3].pin_count = 32;
  pinconfig_metadata.port_info[4].pin_count = 30;

  return pinconfig_metadata;
}()};

}  // namespace

namespace gpio {

class Imx8mGpioTest : public zxtest::Test {
 public:
  Imx8mGpioTest()
      : zxtest::Test(),
        mock_pinmux_regs_(sizeof(uint32_t), 1352),
        mock_gpio1_regs_(sizeof(uint32_t), 32),
        mock_gpio2_regs_(sizeof(uint32_t), 32),
        mock_gpio3_regs_(sizeof(uint32_t), 32),
        mock_gpio4_regs_(sizeof(uint32_t), 32),
        mock_gpio5_regs_(sizeof(uint32_t), 32) {}

  void TearDown() override {
    mock_pinmux_regs_.VerifyAll();
    mock_gpio1_regs_.VerifyAll();
    mock_gpio2_regs_.VerifyAll();
    mock_gpio3_regs_.VerifyAll();
    mock_gpio4_regs_.VerifyAll();
    mock_gpio5_regs_.VerifyAll();
  }

 protected:
  ddk_mock::MockMmioRegRegion mock_pinmux_regs_;
  ddk_mock::MockMmioRegRegion mock_gpio1_regs_;
  ddk_mock::MockMmioRegRegion mock_gpio2_regs_;
  ddk_mock::MockMmioRegRegion mock_gpio3_regs_;
  ddk_mock::MockMmioRegRegion mock_gpio4_regs_;
  ddk_mock::MockMmioRegRegion mock_gpio5_regs_;
};

TEST_F(Imx8mGpioTest, ConfigIn) {
  ddk::MmioBuffer pinmux_mmio = mock_pinmux_regs_.GetMmioBuffer();
  fbl::Vector<ddk::MmioBuffer> gpio_mmios;
  gpio_mmios.push_back(mock_gpio1_regs_.GetMmioBuffer());
  gpio_mmios.push_back(mock_gpio2_regs_.GetMmioBuffer());
  gpio_mmios.push_back(mock_gpio3_regs_.GetMmioBuffer());
  gpio_mmios.push_back(mock_gpio4_regs_.GetMmioBuffer());
  gpio_mmios.push_back(mock_gpio5_regs_.GetMmioBuffer());
  Imx8mGpio dut(nullptr, std::move(pinmux_mmio), std::move(gpio_mmios), {}, kPinConfigMetadata);

  mock_gpio1_regs_[imx8m::kGpioDirReg]
      .ExpectRead(0xffffffff)  // All out by default
      .ExpectWrite(0xfffffffe)
      .ExpectRead(0xfffffffe)
      .ExpectWrite(0xffffeffe);

  mock_gpio2_regs_[imx8m::kGpioDirReg]
      .ExpectRead(0xffffffff)  // All out by default
      .ExpectWrite(0xfffffffe);

  mock_gpio3_regs_[imx8m::kGpioDirReg]
      .ExpectRead(0xffffffff)  // All out by default
      .ExpectWrite(0xfffffff7)
      .ExpectRead(0xfffffff7)
      .ExpectWrite(0xfffff7f7)
      .ExpectRead(0xfffff7f7)
      .ExpectWrite(0xfdfff7f7);

  mock_gpio4_regs_[imx8m::kGpioDirReg]
      .ExpectRead(0xffffffff)  // All out by default
      .ExpectWrite(0xffffbfff)
      .ExpectRead(0xffffbfff)
      .ExpectWrite(0x7fffbfff);

  mock_gpio5_regs_[imx8m::kGpioDirReg]
      .ExpectRead(0xffffffff)  // All out by default
      .ExpectWrite(0xffffff7f);

  // GPIO port 1
  EXPECT_OK(dut.GpioImplConfigIn(0, GPIO_NO_PULL));
  EXPECT_OK(dut.GpioImplConfigIn(12, GPIO_NO_PULL));
  EXPECT_NOT_OK(dut.GpioImplConfigIn(3, GPIO_PULL_DOWN));  // pull settings are not supported
  EXPECT_NOT_OK(dut.GpioImplConfigIn(15, GPIO_PULL_UP));   // pull settings are not supported
  EXPECT_NOT_OK(dut.GpioImplConfigIn(31, GPIO_NO_PULL));   // out of range

  // GPIO port 2
  EXPECT_OK(dut.GpioImplConfigIn(32, GPIO_NO_PULL));
  EXPECT_NOT_OK(dut.GpioImplConfigIn(35, GPIO_PULL_UP));  // pull settings are not supported
  EXPECT_NOT_OK(dut.GpioImplConfigIn(53, GPIO_NO_PULL));  // out of range

  // GPIO port 3
  EXPECT_OK(dut.GpioImplConfigIn(67, GPIO_NO_PULL));
  EXPECT_OK(dut.GpioImplConfigIn(75, GPIO_NO_PULL));
  EXPECT_NOT_OK(dut.GpioImplConfigIn(80, GPIO_PULL_UP));  // pull settings are not supported
  EXPECT_OK(dut.GpioImplConfigIn(89, GPIO_NO_PULL));
  EXPECT_NOT_OK(dut.GpioImplConfigIn(90, GPIO_NO_PULL));  // out of range
  EXPECT_NOT_OK(dut.GpioImplConfigIn(95, GPIO_NO_PULL));  // out of range

  // GPIO port 4
  EXPECT_OK(dut.GpioImplConfigIn(110, GPIO_NO_PULL));
  EXPECT_NOT_OK(dut.GpioImplConfigIn(111, GPIO_PULL_DOWN));  // pull settings are not supported
  EXPECT_OK(dut.GpioImplConfigIn(127, GPIO_NO_PULL));

  // GPIO port 5
  EXPECT_OK(dut.GpioImplConfigIn(135, GPIO_NO_PULL));
  EXPECT_NOT_OK(dut.GpioImplConfigIn(160, GPIO_NO_PULL));  // out of range
}

TEST_F(Imx8mGpioTest, ConfigOut) {
  ddk::MmioBuffer pinmux_mmio = mock_pinmux_regs_.GetMmioBuffer();
  fbl::Vector<ddk::MmioBuffer> gpio_mmios;
  gpio_mmios.push_back(mock_gpio1_regs_.GetMmioBuffer());
  gpio_mmios.push_back(mock_gpio2_regs_.GetMmioBuffer());
  gpio_mmios.push_back(mock_gpio3_regs_.GetMmioBuffer());
  gpio_mmios.push_back(mock_gpio4_regs_.GetMmioBuffer());
  gpio_mmios.push_back(mock_gpio5_regs_.GetMmioBuffer());
  Imx8mGpio dut(nullptr, std::move(pinmux_mmio), std::move(gpio_mmios), {}, kPinConfigMetadata);

  mock_gpio1_regs_[imx8m::kGpioDataReg]
      .ExpectRead(0xffff0000)
      .ExpectWrite(0xffff0001)
      .ExpectRead(0xffff0001)
      .ExpectWrite(0xffff1001)
      .ExpectRead(0xffff1001)
      .ExpectWrite(0xfffe1001);

  mock_gpio1_regs_[imx8m::kGpioDirReg]
      .ExpectRead(0x00000000)  // All are input by default
      .ExpectWrite(0x00000001)
      .ExpectRead(0x00000001)
      .ExpectWrite(0x00001001)
      .ExpectRead(0x00001001)
      .ExpectWrite(0x00011001);

  mock_gpio2_regs_[imx8m::kGpioDataReg].ExpectRead(0xffff0000).ExpectWrite(0xffff0001);

  mock_gpio2_regs_[imx8m::kGpioDirReg]
      .ExpectRead(0x00000000)  // All are input by default
      .ExpectWrite(0x00000001);

  mock_gpio3_regs_[imx8m::kGpioDataReg]
      .ExpectRead(0xffff0000)
      .ExpectWrite(0xffff0008)
      .ExpectRead(0xffff0008)
      .ExpectWrite(0xfdff0008);

  mock_gpio3_regs_[imx8m::kGpioDirReg]
      .ExpectRead(0x00000000)  // All are input by default
      .ExpectWrite(0x00000008)
      .ExpectRead(0x00000008)
      .ExpectWrite(0x02000008);

  mock_gpio4_regs_[imx8m::kGpioDataReg]
      .ExpectRead(0xffff0000)
      .ExpectWrite(0xffff4000)
      .ExpectRead(0xffff4000)
      .ExpectWrite(0x7fff4000);

  mock_gpio4_regs_[imx8m::kGpioDirReg]
      .ExpectRead(0x00000000)  // All are input by default
      .ExpectWrite(0x00004000)
      .ExpectRead(0x00004000)
      .ExpectWrite(0x80004000);

  mock_gpio5_regs_[imx8m::kGpioDataReg].ExpectRead(0xffff0000).ExpectWrite(0xdfff0000);

  mock_gpio5_regs_[imx8m::kGpioDirReg]
      .ExpectRead(0x00000000)  // All are input by default
      .ExpectWrite(0x20000000);

  // GPIO port 1
  EXPECT_OK(dut.GpioImplConfigOut(0, 1));
  EXPECT_OK(dut.GpioImplConfigOut(12, 1));
  EXPECT_OK(dut.GpioImplConfigOut(16, 0));
  EXPECT_NOT_OK(dut.GpioImplConfigOut(31, 1));  // out of range

  // GPIO port 2
  EXPECT_OK(dut.GpioImplConfigOut(32, 1));
  EXPECT_NOT_OK(dut.GpioImplConfigOut(53, 1));  // out of range

  // GPIO port 3
  EXPECT_OK(dut.GpioImplConfigOut(67, 1));
  EXPECT_OK(dut.GpioImplConfigOut(89, 0));
  EXPECT_NOT_OK(dut.GpioImplConfigOut(90, 1));  // out of range
  EXPECT_NOT_OK(dut.GpioImplConfigOut(95, 0));  // out of range

  // GPIO port 4
  EXPECT_OK(dut.GpioImplConfigOut(110, 1));
  EXPECT_OK(dut.GpioImplConfigOut(127, 0));

  // GPIO port 5
  EXPECT_OK(dut.GpioImplConfigOut(157, 0));
  EXPECT_NOT_OK(dut.GpioImplConfigOut(158, 1));  // out of range
  EXPECT_NOT_OK(dut.GpioImplConfigOut(160, 1));  // out of range
}

TEST_F(Imx8mGpioTest, Read) {
  ddk::MmioBuffer pinmux_mmio = mock_pinmux_regs_.GetMmioBuffer();
  fbl::Vector<ddk::MmioBuffer> gpio_mmios;
  gpio_mmios.push_back(mock_gpio1_regs_.GetMmioBuffer());
  gpio_mmios.push_back(mock_gpio2_regs_.GetMmioBuffer());
  gpio_mmios.push_back(mock_gpio3_regs_.GetMmioBuffer());
  gpio_mmios.push_back(mock_gpio4_regs_.GetMmioBuffer());
  gpio_mmios.push_back(mock_gpio5_regs_.GetMmioBuffer());
  Imx8mGpio dut(nullptr, std::move(pinmux_mmio), std::move(gpio_mmios), {}, kPinConfigMetadata);

  mock_gpio1_regs_[imx8m::kGpioDataReg].ExpectRead(0xc6ad7ad8).ExpectRead(0x688608c3);
  mock_gpio2_regs_[imx8m::kGpioDataReg].ExpectRead(0x40cd0cb7).ExpectRead(0x124e597c);
  mock_gpio3_regs_[imx8m::kGpioDataReg].ExpectRead(0x4b174988).ExpectRead(0x59fd2410);
  mock_gpio4_regs_[imx8m::kGpioDataReg].ExpectRead(0x40cd0cb7).ExpectRead(0x124e597c);
  mock_gpio5_regs_[imx8m::kGpioDataReg].ExpectRead(0x4b174988).ExpectRead(0x79fd2410);

  uint8_t value;

  // GPIO port 1
  EXPECT_OK(dut.GpioImplRead(0, &value));
  EXPECT_EQ(0, value);
  EXPECT_OK(dut.GpioImplRead(17, &value));
  EXPECT_EQ(1, value);
  EXPECT_NOT_OK(dut.GpioImplRead(31, &value));

  // GPIO port 2
  EXPECT_OK(dut.GpioImplRead(32, &value));
  EXPECT_EQ(1, value);
  EXPECT_OK(dut.GpioImplRead(47, &value));
  EXPECT_EQ(0, value);
  EXPECT_NOT_OK(dut.GpioImplRead(55, &value));

  // GPIO port 3
  EXPECT_OK(dut.GpioImplRead(69, &value));
  EXPECT_EQ(0, value);
  EXPECT_OK(dut.GpioImplRead(85, &value));
  EXPECT_EQ(1, value);

  // GPIO port 4
  EXPECT_OK(dut.GpioImplRead(112, &value));
  EXPECT_EQ(1, value);
  EXPECT_OK(dut.GpioImplRead(125, &value));
  EXPECT_EQ(0, value);

  // GPIO port 5
  EXPECT_OK(dut.GpioImplRead(128, &value));
  EXPECT_EQ(0, value);
  EXPECT_OK(dut.GpioImplRead(157, &value));
  EXPECT_EQ(1, value);
  EXPECT_NOT_OK(dut.GpioImplRead(158, &value));
}

}  // namespace gpio
