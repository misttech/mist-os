// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/debug.h>
#include <lib/hdmi-dw/hdmi-dw.h>

#include <fbl/array.h>
#include <gtest/gtest.h>
#include <mock-mmio-range/mock-mmio-range.h>

// The MMIO register addresses here are from the Synopsis DesignWare Cores HDMI
// Transmitter Controller Databook, which is distributed by Synopsis.
//
// dwchdmi is version 2.12a, dated April 2016

namespace hdmi_dw {

namespace {

// Register addresses from dwchdmi 6.2 "Interrupt Registers" table 6-14
// "Registers for Address Block: Interrupt"
constexpr int kIhFcStat0Offset = 0x100;
constexpr int kIhFcStat1Offset = 0x101;
constexpr int kIhFcStat2Offset = 0x102;
constexpr int kIhAsStat0Offset = 0x103;
constexpr int kIhPhyStat0Offset = 0x104;
constexpr int kIhI2cmStat0Offset = 0x105;
constexpr int kIhCecStat0Offset = 0x106;
constexpr int kIhVpStat0Offset = 0x107;
constexpr int kIhI2cmphyStat0Offset = 0x108;
constexpr int kIhMuteFcStat0Offset = 0x180;
constexpr int kIhMuteFcStat1Offset = 0x181;
constexpr int kIhMuteFcStat2Offset = 0x182;
constexpr int kIhMuteAsStat0Offset = 0x183;
constexpr int kIhMutePhyStat0Offset = 0x184;
constexpr int kIhMuteI2cmStat0Offset = 0x185;
constexpr int kIhMuteCecStat0Offset = 0x186;
constexpr int kIhMuteVpStat0Offset = 0x187;
constexpr int kIhMuteI2cmphyStat0Offset = 0x188;
constexpr int kIhMuteOffset = 0x1ff;

// Register addresses from dwchdmi 6.3 "VideoSampler Registers" table 6-37
// "Registers for Address Block: VideoSampler"
constexpr int kTxInvid0Offset = 0x200;
constexpr int kTxInstuffingOffset = 0x201;
constexpr int kTxGydata0Offset = 0x202;
constexpr int kTxGydata1Offset = 0x203;
constexpr int kTxRcrdata0Offset = 0x204;
constexpr int kTxRcrdata1Offset = 0x205;
constexpr int kTxBcbdata0Offset = 0x206;
constexpr int kTxBcbdata1Offset = 0x207;

// Register addresses from dwchdmi 6.4 "VideoPacketizer Registers" table 6-46
// "Registers for Address Block: VideoPacketizer"
constexpr int kVpPrCdOffset = 0x801;
constexpr int kVpStuffOffset = 0x802;
constexpr int kVpRemapOffset = 0x803;
constexpr int kVpConfOffset = 0x804;
constexpr int kVpMaskOffset = 0x807;

// Register addresses from dwchdmi 6.5 "FrameComposer Registers" table 6-53
// "Registers for Address Block: FrameComposer"
constexpr int kFcInvidconfOffset = 0x1000;
constexpr int kFcInhactiv0Offset = 0x1001;
constexpr int kFcInhactiv1Offset = 0x1002;
constexpr int kFcInhblank0Offset = 0x1003;
constexpr int kFcInhblank1Offset = 0x1004;
constexpr int kFcInvactiv0Offset = 0x1005;
constexpr int kFcInvactiv1Offset = 0x1006;
constexpr int kFcInvblankOffset = 0x1007;
constexpr int kFcHsyncindelay0Offset = 0x1008;
constexpr int kFcHsyncindelay1Offset = 0x1009;
constexpr int kFcHsyncinwidth0Offset = 0x100a;
constexpr int kFcHsyncinwidth1Offset = 0x100b;
constexpr int kFcVsyncindelayOffset = 0x100c;
constexpr int kFcVsyncinwidthOffset = 0x100d;
constexpr int kFcCtrldurOffset = 0x1011;
constexpr int kFcExctrldurOffset = 0x1012;
constexpr int kFcExctrlspacOffset = 0x1013;
constexpr int kFcAviconf3Offset = 0x1017;
constexpr int kFcGcpOffset = 0x1018;
constexpr int kFcAviconf0Offset = 0x1019;
constexpr int kFcAviconf1Offset = 0x101a;
constexpr int kFcAviconf2Offset = 0x101b;
constexpr int kFcMask0Offset = 0x10d2;
constexpr int kFcMask1Offset = 0x10d6;
constexpr int kFcMask2Offset = 0x10da;
constexpr int kFcPrconfOffset = 0x10e0;
constexpr int kFcScamblerCtrlOffset = 0x10e1;
constexpr int kFcActspcHdlrCfgOffset = 0x10e8;
constexpr int kFcInvact2d0Offset = 0x10e9;
constexpr int kFcInvact2d1Offset = 0x10ea;

// Register addresses from dwchdmi 6.12 "MainController Registers" table 6-317
// "Registers for Address Block: Controller"
constexpr int kMcClkdisOffset = 0x4001;
constexpr int kMcSwrstzreqOffset = 0x4002;
constexpr int kMcFlowctrlOffset = 0x4004;
constexpr int kMcLockonclockOffset = 0x4006;

// Register addresses from dwchdmi 6.13 "ColorSpaceConverter Registers" table
// 6-327 "Registers for Address Block: ColorSpaceConverter"
constexpr int kCscCfgOffset = 0x4100;
constexpr int kCscScaleOffset = 0x4101;
constexpr int kCscCoefA1MsbOffset = 0x4102;
constexpr int kCscCoefA1LsbOffset = 0x4103;
constexpr int kCscCoefA2MsbOffset = 0x4104;
constexpr int kCscCoefA2LsbOffset = 0x4105;
constexpr int kCscCoefA3MsbOffset = 0x4106;
constexpr int kCscCoefA3LsbOffset = 0x4107;
constexpr int kCscCoefA4MsbOffset = 0x4108;
constexpr int kCscCoefA4LsbOffset = 0x4109;
constexpr int kCscCoefB1MsbOffset = 0x410a;
constexpr int kCscCoefB1LsbOffset = 0x410b;
constexpr int kCscCoefB2MsbOffset = 0x410c;
constexpr int kCscCoefB2LsbOffset = 0x410d;
constexpr int kCscCoefB3MsbOffset = 0x410e;
constexpr int kCscCoefB3LsbOffset = 0x410f;
constexpr int kCscCoefB4MsbOffset = 0x4110;
constexpr int kCscCoefB4LsbOffset = 0x4111;
constexpr int kCscCoefC1MsbOffset = 0x4112;
constexpr int kCscCoefC1LsbOffset = 0x4113;
constexpr int kCscCoefC2MsbOffset = 0x4114;
constexpr int kCscCoefC2LsbOffset = 0x4115;
constexpr int kCscCoefC3MsbOffset = 0x4116;
constexpr int kCscCoefC3LsbOffset = 0x4117;
constexpr int kCscCoefC4MsbOffset = 0x4118;
constexpr int kCscCoefC4LsbOffset = 0x4119;

// Register addresses from dwchdmi 6.14 "HDCP Registers" table 6-358 "Registers
// for Address Block: HDCP"
constexpr int kAApiintclrOffset = 0x5006;

// Register addresses from dwchdmi 6.15 "HDCP22 Registers" table 6-405
// "Registers for Address Block: HDCP22"
constexpr int kHdcp22regStatOffset = 0x790d;

// Register addresses from dwchdmi 6.17 "EDDC Registers" table 6-424 "Registers
// for Address Block: EDDC"
//
// The register names here reflect the updated I2C naming convention, adopted in
// I2C specification revision 1.7.
constexpr int kI2cmTargetOffset = 0x7e00;
constexpr int kI2cmAddressOffset = 0x7e01;
constexpr int kI2cmDataoOffset = 0x7e02;
constexpr int kI2cmDataiOffset = 0x7e03;
constexpr int kI2cmOperationOffset = 0x7e04;
constexpr int kI2cmIntOffset = 0x7e05;
constexpr int kI2cmCtlintOffset = 0x7e06;
constexpr int kI2cmDivOffset = 0x7e07;
constexpr int kI2cmSegAddrOffset = 0x7e08;
constexpr int kI2cmSegPtrOffset = 0x7e0a;
constexpr int kI2cmSsSclHcnt1AddrOffset = 0x7e0b;
constexpr int kI2cmSsSclHcnt0AddrOffset = 0x7e0c;
constexpr int kI2cmSsSclLcnt1AddrOffset = 0x7e0d;
constexpr int kI2cmSsSclLcnt0AddrOffset = 0x7e0e;
constexpr int kI2cmFsSclHcnt1AddrOffset = 0x7e0f;
constexpr int kI2cmFsSclHcnt0AddrOffset = 0x7e10;
constexpr int kI2cmFsSclLcnt1AddrOffset = 0x7e11;
constexpr int kI2cmFsSclLcnt0AddrOffset = 0x7e12;
constexpr int kI2cmSdaHoldOffset = 0x7e13;
constexpr int kI2cmScdcReadUpdateOffset = 0x7e14;
constexpr int kI2cmReadBuff0Offset = 0x7e20;

using fuchsia_hardware_hdmi::wire::ColorDepth;
using fuchsia_hardware_hdmi::wire::ColorFormat;
using fuchsia_hardware_hdmi::wire::ColorParam;
using fuchsia_hardware_hdmi::wire::StandardDisplayMode;

class FakeHdmiIpBase : public HdmiIpBase {
 public:
  explicit FakeHdmiIpBase(fdf::MmioBuffer mmio) : HdmiIpBase(), mmio_(std::move(mmio)) {}

  void WriteIpReg(uint32_t addr, uint32_t data) { mmio_.Write8(data, addr); }
  uint32_t ReadIpReg(uint32_t addr) { return mmio_.Read8(addr); }

 private:
  fdf::MmioBuffer mmio_;
};

class FakeHdmiDw : public HdmiDw {
 public:
  static std::unique_ptr<FakeHdmiDw> Create(fdf::MmioBuffer mmio) {
    return std::make_unique<FakeHdmiDw>(std::move(mmio));
  }

  explicit FakeHdmiDw(fdf::MmioBuffer mmio) : HdmiDw(&base_), base_(std::move(mmio)) {}

 private:
  FakeHdmiIpBase base_;
};

class HdmiDwTest : public testing::Test {
 public:
  void SetUp() override { hdmi_dw_ = FakeHdmiDw::Create(mmio_range_.GetMmioBuffer()); }

  void TearDown() override { mmio_range_.CheckAllAccessesReplayed(); }

  void ExpectScdcWrite(uint8_t address, uint8_t value) {
    mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
        {.address = kI2cmTargetOffset, .value = 0x54, .write = true},
        {.address = kI2cmAddressOffset, .value = address, .write = true},
        {.address = kI2cmDataoOffset, .value = value, .write = true},
        {.address = kI2cmOperationOffset, .value = 0b01'0000, .write = true},
    }));
  }

  void ExpectScdcRead(uint8_t address, uint8_t value) {
    mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
        {.address = kI2cmTargetOffset, .value = 0x54, .write = true},
        {.address = kI2cmAddressOffset, .value = address, .write = true},
        {.address = kI2cmOperationOffset, .value = 0b00'0001, .write = true},
        {.address = kI2cmDataiOffset, .value = value},
    }));
  }

 protected:
  constexpr static int kMmioRangeSize = 0x10000;
  ddk_mock::MockMmioRange mmio_range_{kMmioRangeSize, ddk_mock::MockMmioRange::Size::k8};

  std::unique_ptr<FakeHdmiDw> hdmi_dw_;
};

TEST_F(HdmiDwTest, InitHwTest) {
  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = kMcLockonclockOffset, .value = 0b1111'1111, .write = true},
      {.address = kMcClkdisOffset, .value = 0b0000'0000, .write = true},

      {.address = kI2cmIntOffset, .value = 0b0000'0000, .write = true},
      {.address = kI2cmCtlintOffset, .value = 0b0000'0000, .write = true},
      {.address = kI2cmDivOffset, .value = 0b0000'0000, .write = true},

      {.address = kI2cmSsSclHcnt1AddrOffset, .value = 0x00, .write = true},
      {.address = kI2cmSsSclHcnt0AddrOffset, .value = 0xcf, .write = true},
      {.address = kI2cmSsSclLcnt1AddrOffset, .value = 0x00, .write = true},
      {.address = kI2cmSsSclLcnt0AddrOffset, .value = 0xff, .write = true},
      {.address = kI2cmFsSclHcnt1AddrOffset, .value = 0x00, .write = true},
      {.address = kI2cmFsSclHcnt0AddrOffset, .value = 0x0f, .write = true},
      {.address = kI2cmFsSclLcnt1AddrOffset, .value = 0x00, .write = true},
      {.address = kI2cmFsSclLcnt0AddrOffset, .value = 0x20, .write = true},

      {.address = kI2cmSdaHoldOffset, .value = 0x08, .write = true},
      {.address = kI2cmScdcReadUpdateOffset, .value = 0b0000'0000, .write = true},
  }));

  hdmi_dw_->InitHw();
}

TEST_F(HdmiDwTest, EdidTransferTest) {
  uint8_t in_data[] = {1, 2};
  uint8_t out_data[16] = {0};
  i2c_impl_op_t op_list[]{
      {
          .address = 0x30,
          .data_buffer = &in_data[0],
          .data_size = 1,
          .is_read = false,
          .stop = false,
      },
      {
          .address = 0x50,
          .data_buffer = &in_data[1],
          .data_size = 1,
          .is_read = false,
          .stop = false,
      },
      {
          .address = 0x50,
          .data_buffer = &out_data[0],
          .data_size = 16,
          .is_read = true,
          .stop = true,
      },
  };

  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = kI2cmTargetOffset, .value = 0x50, .write = true},
      {.address = kI2cmSegAddrOffset, .value = 0x30, .write = true},
      {.address = kI2cmSegPtrOffset, .value = 0x01, .write = true},
      {.address = kI2cmAddressOffset, .value = 2, .write = true},
      {.address = kI2cmOperationOffset, .value = 0b00'0100, .write = true},

      {.address = kIhI2cmStat0Offset, .value = 0b0000'0000},
      {.address = kIhI2cmStat0Offset, .value = 0b1111'1111},
      {.address = kIhI2cmStat0Offset, .value = 0b010, .write = true},

      {.address = kI2cmReadBuff0Offset + 0, .value = 8},
      {.address = kI2cmReadBuff0Offset + 1, .value = 7},
      {.address = kI2cmReadBuff0Offset + 2, .value = 6},
      {.address = kI2cmReadBuff0Offset + 3, .value = 5},
      {.address = kI2cmReadBuff0Offset + 4, .value = 4},
      {.address = kI2cmReadBuff0Offset + 5, .value = 3},
      {.address = kI2cmReadBuff0Offset + 6, .value = 2},
      {.address = kI2cmReadBuff0Offset + 7, .value = 1},

      {.address = kI2cmAddressOffset, .value = 10, .write = true},
      {.address = kI2cmOperationOffset, .value = 0b00'0100, .write = true},

      {.address = kIhI2cmStat0Offset, .value = 0b1111'1111},
      {.address = kIhI2cmStat0Offset, .value = 0b010, .write = true},

      {.address = kI2cmReadBuff0Offset + 0, .value = 1},
      {.address = kI2cmReadBuff0Offset + 1, .value = 2},
      {.address = kI2cmReadBuff0Offset + 2, .value = 3},
      {.address = kI2cmReadBuff0Offset + 3, .value = 4},
      {.address = kI2cmReadBuff0Offset + 4, .value = 5},
      {.address = kI2cmReadBuff0Offset + 5, .value = 6},
      {.address = kI2cmReadBuff0Offset + 6, .value = 7},
      {.address = kI2cmReadBuff0Offset + 7, .value = 8},
  }));

  hdmi_dw_->EdidTransfer(op_list, sizeof(op_list) / sizeof(op_list[0]));
  uint8_t expected_out[] = {8, 7, 6, 5, 4, 3, 2, 1, 1, 2, 3, 4, 5, 6, 7, 8};
  for (uint32_t i = 0; i < 16; i++) {
    EXPECT_EQ(out_data[i], expected_out[i]);
  }
}

TEST_F(HdmiDwTest, ConfigHdmitxTest) {
  fidl::Arena allocator;
  StandardDisplayMode standard_display_mode{
      .pixel_clock_10khz = 30,
      .h_addressable = 24,
      .h_front_porch = 15,
      .h_sync_pulse = 50,
      .h_blanking = 93,
      .v_addressable = 75,
      .v_front_porch = 104,
      .v_sync_pulse = 49,
      .v_blanking = 83,
      .flags = 0,
  };
  ColorParam color{
      .input_color_format = ColorFormat::kCfRgb,
      .output_color_format = ColorFormat::kCf444,
      .color_depth = ColorDepth::kCd30B,
  };
  DisplayMode mode(allocator);
  mode.set_mode(allocator, standard_display_mode);
  mode.set_color(color);

  hdmi_param_tx p{
      .vic = 9,
      .aspect_ratio = 0,
      .colorimetry = 1,
      .is4K = false,
  };

  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = kTxInvid0Offset, .value = 0x03, .write = true},

      {.address = kTxInstuffingOffset, .value = 0b000, .write = true},
      {.address = kTxGydata0Offset, .value = 0x00, .write = true},
      {.address = kTxGydata1Offset, .value = 0x00, .write = true},
      {.address = kTxRcrdata0Offset, .value = 0x00, .write = true},
      {.address = kTxRcrdata1Offset, .value = 0x00, .write = true},
      {.address = kTxBcbdata0Offset, .value = 0x00, .write = true},
      {.address = kTxBcbdata1Offset, .value = 0x00, .write = true},

      // ConfigCsc
      {.address = kMcFlowctrlOffset, .value = 0x01, .write = true},

      {.address = kCscCfgOffset, .value = 0b0000'0000, .write = true},

      {.address = kCscCoefA1MsbOffset, .value = 0x25, .write = true},
      {.address = kCscCoefA1LsbOffset, .value = 0x91, .write = true},
      {.address = kCscCoefA2MsbOffset, .value = 0x13, .write = true},
      {.address = kCscCoefA2LsbOffset, .value = 0x23, .write = true},
      {.address = kCscCoefA3MsbOffset, .value = 0x07, .write = true},
      {.address = kCscCoefA3LsbOffset, .value = 0x4c, .write = true},
      {.address = kCscCoefA4MsbOffset, .value = 0x00, .write = true},
      {.address = kCscCoefA4LsbOffset, .value = 0x00, .write = true},

      {.address = kCscCoefB1MsbOffset, .value = 0xe5, .write = true},
      {.address = kCscCoefB1LsbOffset, .value = 0x34, .write = true},
      {.address = kCscCoefB2MsbOffset, .value = 0x20, .write = true},
      {.address = kCscCoefB2LsbOffset, .value = 0x00, .write = true},
      {.address = kCscCoefB3MsbOffset, .value = 0xfa, .write = true},
      {.address = kCscCoefB3LsbOffset, .value = 0xcc, .write = true},
      {.address = kCscCoefB4MsbOffset, .value = 0x08, .write = true},
      {.address = kCscCoefB4LsbOffset, .value = 0x00, .write = true},

      {.address = kCscCoefC1MsbOffset, .value = 0xea, .write = true},
      {.address = kCscCoefC1LsbOffset, .value = 0xcd, .write = true},
      {.address = kCscCoefC2MsbOffset, .value = 0xf5, .write = true},
      {.address = kCscCoefC2LsbOffset, .value = 0x33, .write = true},
      {.address = kCscCoefC3MsbOffset, .value = 0x20, .write = true},
      {.address = kCscCoefC3LsbOffset, .value = 0x00, .write = true},
      {.address = kCscCoefC4MsbOffset, .value = 0x08, .write = true},
      {.address = kCscCoefC4LsbOffset, .value = 0x00, .write = true},

      {.address = kCscScaleOffset, .value = 0b0101'0000, .write = true},
      // ConfigCsc end

      {.address = kVpPrCdOffset, .value = 0b0000'0000, .write = true},
      {.address = kVpStuffOffset, .value = 0b00'0000, .write = true},
      {.address = kVpRemapOffset, .value = 0b00, .write = true},
      {.address = kVpConfOffset, .value = 0b100'0110, .write = true},
      {.address = kVpMaskOffset, .value = 0b1111'1111, .write = true},

      {.address = kFcInvidconfOffset, .value = 0b1111'1000, .write = true},

      {.address = kFcInhactiv0Offset, .value = 24, .write = true},
      {.address = kFcInhactiv1Offset, .value = 0, .write = true},
      {.address = kFcInhblank0Offset, .value = 93, .write = true},
      {.address = kFcInhblank1Offset, .value = 0, .write = true},
      {.address = kFcInvactiv0Offset, .value = 75, .write = true},
      {.address = kFcInvactiv1Offset, .value = 0, .write = true},
      {.address = kFcInvblankOffset, .value = 83, .write = true},
      {.address = kFcHsyncindelay0Offset, .value = 15, .write = true},
      {.address = kFcHsyncindelay1Offset, .value = 0, .write = true},
      {.address = kFcHsyncinwidth0Offset, .value = 50, .write = true},
      {.address = kFcHsyncinwidth1Offset, .value = 0, .write = true},
      {.address = kFcVsyncindelayOffset, .value = 104, .write = true},
      {.address = kFcVsyncinwidthOffset, .value = 49, .write = true},

      {.address = kFcCtrldurOffset, .value = 12, .write = true},
      {.address = kFcExctrldurOffset, .value = 32, .write = true},
      {.address = kFcExctrlspacOffset, .value = 1, .write = true},

      {.address = kFcGcpOffset, .value = 0b001, .write = true},
      {.address = kFcAviconf0Offset, .value = 0b0100'0010, .write = true},
      {.address = kFcAviconf1Offset, .value = 0b0100'1000, .write = true},
      {.address = kFcAviconf2Offset, .value = 0b0000'0000, .write = true},
      {.address = kFcAviconf3Offset, .value = 0b0000, .write = true},

      {.address = kFcActspcHdlrCfgOffset, .value = 0b00, .write = true},
      {.address = kFcInvact2d0Offset, .value = 75, .write = true},
      {.address = kFcInvact2d1Offset, .value = 0, .write = true},

      {.address = kFcMask0Offset, .value = 0b1110'0111, .write = true},
      {.address = kFcMask1Offset, .value = 0b1111'1011, .write = true},
      {.address = kFcMask2Offset, .value = 0b0'0011, .write = true},

      {.address = kFcPrconfOffset, .value = 0x10, .write = true},

      {.address = kIhFcStat0Offset, .value = 0b1111'1111, .write = true},
      {.address = kIhFcStat1Offset, .value = 0b1111'1111, .write = true},
      {.address = kIhFcStat2Offset, .value = 0b1111'1111, .write = true},
      {.address = kIhAsStat0Offset, .value = 0b1111'1111, .write = true},
      {.address = kIhPhyStat0Offset, .value = 0b1111'1111, .write = true},
      {.address = kIhI2cmStat0Offset, .value = 0b1111'1111, .write = true},
      {.address = kIhCecStat0Offset, .value = 0b1111'1111, .write = true},
      {.address = kIhVpStat0Offset, .value = 0b1111'1111, .write = true},
      {.address = kIhI2cmphyStat0Offset, .value = 0b1111'1111, .write = true},
      {.address = kAApiintclrOffset, .value = 0b1111'1111, .write = true},
      {.address = kHdcp22regStatOffset, .value = 0b1111'1111, .write = true},
  }));

  hdmi_dw_->ConfigHdmitx(mode, p);
}

TEST_F(HdmiDwTest, SetupInterruptsTest) {
  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = kIhMuteFcStat0Offset, .value = 0b1111'1111, .write = true},
      {.address = kIhMuteFcStat1Offset, .value = 0b1111'1111, .write = true},
      {.address = kIhMuteFcStat2Offset, .value = 0b0'0011, .write = true},

      {.address = kIhMuteAsStat0Offset, .value = 0b0'0111, .write = true},
      {.address = kIhMutePhyStat0Offset, .value = 0b11'1111, .write = true},
      {.address = kIhMuteI2cmStat0Offset, .value = 0b010, .write = true},
      {.address = kIhMuteCecStat0Offset, .value = 0b000'0000, .write = true},
      {.address = kIhMuteVpStat0Offset, .value = 0b1111'1111, .write = true},
      {.address = kIhMuteI2cmphyStat0Offset, .value = 0b11, .write = true},

      {.address = kIhMuteOffset, .value = 0b00, .write = true},
  }));

  hdmi_dw_->SetupInterrupts();
}

TEST_F(HdmiDwTest, ResetTest) {
  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = kMcSwrstzreqOffset, .value = 0b0000'0000, .write = true},
      {.address = kMcSwrstzreqOffset, .value = 0b0111'1101, .write = true},
      {.address = kFcVsyncinwidthOffset, .value = 0x41},
      {.address = kFcVsyncinwidthOffset, .value = 0x41, .write = true},

      {.address = kMcClkdisOffset, .value = 0b00, .write = true},
  }));

  hdmi_dw_->Reset();
}

TEST_F(HdmiDwTest, SetupScdcTest) {
  // is4k = true
  ExpectScdcRead(0x1, 0);
  ExpectScdcWrite(0x2, 0x1);
  ExpectScdcWrite(0x2, 0x1);

  ExpectScdcWrite(0x20, 0x3);
  ExpectScdcWrite(0x20, 0x3);

  hdmi_dw_->SetupScdc(true);

  // is4k = false
  ExpectScdcRead(0x1, 0);
  ExpectScdcWrite(0x2, 0x1);
  ExpectScdcWrite(0x2, 0x1);

  ExpectScdcWrite(0x20, 0x0);
  ExpectScdcWrite(0x20, 0x0);

  hdmi_dw_->SetupScdc(false);
}

TEST_F(HdmiDwTest, ResetFcTest) {
  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = kFcInvidconfOffset, .value = 0b1111'1111},
      {.address = kFcInvidconfOffset, .value = 0b1111'0111, .write = true},
      {.address = kFcInvidconfOffset, .value = 0b0000'0000},
      {.address = kFcInvidconfOffset, .value = 0b0000'1000, .write = true},
  }));

  hdmi_dw_->ResetFc();
}

TEST_F(HdmiDwTest, SetFcScramblerCtrlTest) {
  // is4k = true
  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = kFcScamblerCtrlOffset, .value = 0b0000'0000},
      {.address = kFcScamblerCtrlOffset, .value = 0b0000'0001, .write = true},
  }));
  hdmi_dw_->SetFcScramblerCtrl(true);

  // is4k = false
  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = kFcScamblerCtrlOffset, .value = 0b0000'0000, .write = true},
  }));

  hdmi_dw_->SetFcScramblerCtrl(false);
}

}  // namespace

}  // namespace hdmi_dw
