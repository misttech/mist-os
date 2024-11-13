// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/designware-hdmi/hdmi-transmitter-controller-impl.h"

#include <lib/driver/logging/cpp/logger.h>
#include <unistd.h>
#include <zircon/assert.h>

#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/designware-hdmi/color-param.h"
#include "src/graphics/display/lib/designware-hdmi/regs.h"

namespace designware_hdmi {

void HdmiTransmitterControllerImpl::ScdcWrite(uint8_t addr, uint8_t val) {
  WriteReg(HDMITX_DWC_I2CM_SLAVE, 0x54);
  WriteReg(HDMITX_DWC_I2CM_ADDRESS, addr);
  WriteReg(HDMITX_DWC_I2CM_DATAO, val);
  WriteReg(HDMITX_DWC_I2CM_OPERATION, 0x10);
  usleep(2000);
}

void HdmiTransmitterControllerImpl::ScdcRead(uint8_t addr, uint8_t* val) {
  WriteReg(HDMITX_DWC_I2CM_SLAVE, 0x54);
  WriteReg(HDMITX_DWC_I2CM_ADDRESS, addr);
  WriteReg(HDMITX_DWC_I2CM_OPERATION, 1);
  usleep(2000);
  *val = (uint8_t)ReadReg(HDMITX_DWC_I2CM_DATAI);
}

zx_status_t HdmiTransmitterControllerImpl::InitHw() {
  WriteReg(HDMITX_DWC_MC_LOCKONCLOCK, 0xff);
  WriteReg(HDMITX_DWC_MC_CLKDIS, 0x00);

  /* Step 2: Initialize DDC Interface (For EDID) */

  // FIXME: Pinmux i2c pins (skip for now since uboot it doing it)

  // Configure i2c interface
  // a. disable all interrupts (read_req, done, nack, arbitration)
  WriteReg(HDMITX_DWC_I2CM_INT, 0);
  WriteReg(HDMITX_DWC_I2CM_CTLINT, 0);

  // b. set interface to standard mode
  WriteReg(HDMITX_DWC_I2CM_DIV, 0);

  // c. Setup i2c timings (based on u-boot source)
  WriteReg(HDMITX_DWC_I2CM_SS_SCL_HCNT_1, 0);
  WriteReg(HDMITX_DWC_I2CM_SS_SCL_HCNT_0, 0xcf);
  WriteReg(HDMITX_DWC_I2CM_SS_SCL_LCNT_1, 0);
  WriteReg(HDMITX_DWC_I2CM_SS_SCL_LCNT_0, 0xff);
  WriteReg(HDMITX_DWC_I2CM_FS_SCL_HCNT_1, 0);
  WriteReg(HDMITX_DWC_I2CM_FS_SCL_HCNT_0, 0x0f);
  WriteReg(HDMITX_DWC_I2CM_FS_SCL_LCNT_1, 0);
  WriteReg(HDMITX_DWC_I2CM_FS_SCL_LCNT_0, 0x20);
  WriteReg(HDMITX_DWC_I2CM_SDA_HOLD, 0x08);

  // d. disable any SCDC operations for now
  WriteReg(HDMITX_DWC_I2CM_SCDC_UPDATE, 0);

  return ZX_OK;
}

void HdmiTransmitterControllerImpl::ConfigHdmitx(const ColorParam& color_param,
                                                 const display::DisplayTiming& mode,
                                                 const hdmi_param_tx& p) {
  // setup video input mapping
  uint8_t video_input_mapping_config = 0;
  if (color_param.input_color_format == ColorFormat::kCfRgb) {
    switch (color_param.color_depth) {
      case ColorDepth::kCd24B:
        video_input_mapping_config |= TX_INVID0_VM_RGB444_8B;
        break;
      case ColorDepth::kCd30B:
        video_input_mapping_config |= TX_INVID0_VM_RGB444_10B;
        break;
      case ColorDepth::kCd36B:
        video_input_mapping_config |= TX_INVID0_VM_RGB444_12B;
        break;
      case ColorDepth::kCd48B:
      default:
        video_input_mapping_config |= TX_INVID0_VM_RGB444_16B;
        break;
    }
  } else if (color_param.input_color_format == ColorFormat::kCf444) {
    switch (color_param.color_depth) {
      case ColorDepth::kCd24B:
        video_input_mapping_config |= TX_INVID0_VM_YCBCR444_8B;
        break;
      case ColorDepth::kCd30B:
        video_input_mapping_config |= TX_INVID0_VM_YCBCR444_10B;
        break;
      case ColorDepth::kCd36B:
        video_input_mapping_config |= TX_INVID0_VM_YCBCR444_12B;
        break;
      case ColorDepth::kCd48B:
      default:
        video_input_mapping_config |= TX_INVID0_VM_YCBCR444_16B;
        break;
    }
  } else {
    ZX_DEBUG_ASSERT_MSG(false, "Invalid display input color format: %d",
                        static_cast<uint8_t>(color_param.input_color_format));
    return;
  }
  WriteReg(HDMITX_DWC_TX_INVID0, video_input_mapping_config);

  // Disable video input stuffing and zero-out related registers
  WriteReg(HDMITX_DWC_TX_INSTUFFING, 0x00);
  WriteReg(HDMITX_DWC_TX_GYDATA0, 0x00);
  WriteReg(HDMITX_DWC_TX_GYDATA1, 0x00);
  WriteReg(HDMITX_DWC_TX_RCRDATA0, 0x00);
  WriteReg(HDMITX_DWC_TX_RCRDATA1, 0x00);
  WriteReg(HDMITX_DWC_TX_BCBDATA0, 0x00);
  WriteReg(HDMITX_DWC_TX_BCBDATA1, 0x00);

  // configure CSC (Color Space Converter)
  ConfigCsc(color_param);

  // Video packet color depth and pixel repetition (none). writing 0 is also valid
  // hdmi_data = (4 << 4); // 4 == 24bit
  // hdmi_data = (display->color_depth << 4); // 4 == 24bit
  WriteReg(HDMITX_DWC_VP_PR_CD, (0 << 4));  // 4 == 24bit

  // setup video packet stuffing (nothing fancy to be done here)
  WriteReg(HDMITX_DWC_VP_STUFF, 0);

  // setup video packet remap (nothing here as well since we don't support 422)
  WriteReg(HDMITX_DWC_VP_REMAP, 0);

  // vp packet output configuration
  const uint8_t vp_packet_configuration =
      VP_CONF_BYPASS_EN | VP_CONF_BYPASS_SEL_VP | VP_CONF_OUTSELECTOR;
  WriteReg(HDMITX_DWC_VP_CONF, vp_packet_configuration);

  // Video packet Interrupt Mask
  WriteReg(HDMITX_DWC_VP_MASK, 0xFF);  // set all bits

  // TODO: For now skip audio configuration

  // Setup frame composer

  // fc_invidconf setup
  uint8_t input_video_configuration =
      FC_INVIDCONF_HDCP_KEEPOUT | FC_INVIDCONF_VSYNC_POL(mode->flags & ModeFlag::kVsyncPositive) |
      FC_INVIDCONF_HSYNC_POL(mode->flags & ModeFlag::kHsyncPositive) | FC_INVIDCONF_DE_POL_H |
      FC_INVIDCONF_DVI_HDMI_MODE;
  if (mode.fields_per_frame == display::FieldsPerFrame::kInterlaced) {
    input_video_configuration |= FC_INVIDCONF_VBLANK_OSC | FC_INVIDCONF_IN_VID_INTERLACED;
  }
  WriteReg(HDMITX_DWC_FC_INVIDCONF, input_video_configuration);

  // TODO(https://fxbug.dev/325994853): Add a configuration on the display
  // timings and make the ZX_ASSERT() checks below preconditions of
  // ConfigHdmiTx.

  // HActive
  const int horizontal_active_px = mode.horizontal_active_px;
  ZX_ASSERT(horizontal_active_px <= 0x3fff);
  WriteReg(HDMITX_DWC_FC_INHACTV0, (horizontal_active_px & 0xff));
  WriteReg(HDMITX_DWC_FC_INHACTV1, ((horizontal_active_px >> 8) & 0x3f));

  // HBlank
  const int horizontal_blank_px = mode.horizontal_blank_px();
  ZX_ASSERT(horizontal_blank_px <= 0x1fff);
  WriteReg(HDMITX_DWC_FC_INHBLANK0, (horizontal_blank_px & 0xff));
  WriteReg(HDMITX_DWC_FC_INHBLANK1, ((horizontal_blank_px >> 8) & 0x1f));

  // VActive
  const int vertical_active_lines = mode.vertical_active_lines;
  ZX_ASSERT(vertical_active_lines <= 0x1fff);
  WriteReg(HDMITX_DWC_FC_INVACTV0, (vertical_active_lines & 0xff));
  WriteReg(HDMITX_DWC_FC_INVACTV1, ((vertical_active_lines >> 8) & 0x1f));

  // VBlank
  const int vertical_blank_lines = mode.vertical_blank_lines();
  ZX_ASSERT(vertical_blank_lines <= 0xff);
  WriteReg(HDMITX_DWC_FC_INVBLANK, (vertical_blank_lines & 0xff));

  // HFP
  const int horizontal_front_porch_px = mode.horizontal_front_porch_px;
  ZX_ASSERT(horizontal_front_porch_px <= 0x1fff);
  WriteReg(HDMITX_DWC_FC_HSYNCINDELAY0, (horizontal_front_porch_px & 0xff));
  WriteReg(HDMITX_DWC_FC_HSYNCINDELAY1, ((horizontal_front_porch_px >> 8) & 0x1f));

  // HSync
  const int horizontal_sync_width_px = mode.horizontal_sync_width_px;
  ZX_ASSERT(horizontal_sync_width_px <= 0x3ff);
  WriteReg(HDMITX_DWC_FC_HSYNCINWIDTH0, (horizontal_sync_width_px & 0xff));
  WriteReg(HDMITX_DWC_FC_HSYNCINWIDTH1, ((horizontal_sync_width_px >> 8) & 0x3));

  // VFront
  const int vertical_front_porch_lines = mode.vertical_front_porch_lines;
  ZX_ASSERT(vertical_front_porch_lines <= 0xff);
  WriteReg(HDMITX_DWC_FC_VSYNCINDELAY, (vertical_front_porch_lines & 0xff));

  // VSync
  const int vertical_sync_width_lines = mode.vertical_sync_width_lines;
  ZX_ASSERT(vertical_sync_width_lines <= 0x3f);
  WriteReg(HDMITX_DWC_FC_VSYNCINWIDTH, (vertical_sync_width_lines & 0x3f));

  // Frame Composer control period duration (set to 12 per spec)
  WriteReg(HDMITX_DWC_FC_CTRLDUR, 12);

  // Frame Composer extended control period duration (set to 32 per spec)
  WriteReg(HDMITX_DWC_FC_EXCTRLDUR, 32);

  // Frame Composer extended control period max spacing (FIXME: spec says 50, uboot sets to 1)
  WriteReg(HDMITX_DWC_FC_EXCTRLSPAC, 1);

  // Frame Composer preamble filler (from uBoot)

  // Frame Composer GCP packet config
  WriteReg(HDMITX_DWC_FC_GCP, (1 << 0));  // set avmute. defauly_phase is 0

  // Frame Composer AVI Packet config (set active_format_present bit)
  // aviconf0 populates Table 10 of CEA spec (AVI InfoFrame Data Byte 1)
  // Y1Y0 = 00 for RGB, 10 for 444
  if (color_param.output_color_format == ColorFormat::kCfRgb) {
    video_input_mapping_config = FC_AVICONF0_RGB;
  } else {
    video_input_mapping_config = FC_AVICONF0_444;
  }
  // A0 = 1 Active Formate present on R3R0
  video_input_mapping_config |= FC_AVICONF0_A0;
  WriteReg(HDMITX_DWC_FC_AVICONF0, video_input_mapping_config);

  // aviconf1 populates Table 11 of AVI InfoFrame Data Byte 2
  // C1C0 = 0, M1M0=0x2 (16:9), R3R2R1R0=0x8 (same of M1M0)
  video_input_mapping_config = FC_AVICONF1_R3R0;  // set to 0x8 (same as coded frame aspect ratio)
  video_input_mapping_config |= FC_AVICONF1_M1M0(static_cast<uint8_t>(p.aspect_ratio));
  video_input_mapping_config |= FC_AVICONF1_C1C0(static_cast<uint8_t>(p.colorimetry));
  WriteReg(HDMITX_DWC_FC_AVICONF1, video_input_mapping_config);

  // Since we are support RGB/444, no need to write to ECx
  WriteReg(HDMITX_DWC_FC_AVICONF2, 0x0);

  // YCC and IT Quantizations according to CEA spec (limited range for now)
  WriteReg(HDMITX_DWC_FC_AVICONF3, 0x0);

  // Set AVI InfoFrame VIC
  // WriteReg(HDMITX_DWC_FC_AVIVID, (p->vic >= VESA_OFFSET)? 0 : p->vic);

  WriteReg(HDMITX_DWC_FC_ACTSPC_HDLR_CFG, 0);

  // Frame composer 2d vact config
  ZX_ASSERT(vertical_active_lines <= 0xfff);
  WriteReg(HDMITX_DWC_FC_INVACT_2D_0, (vertical_active_lines & 0xff));
  WriteReg(HDMITX_DWC_FC_INVACT_2D_1, ((vertical_active_lines >> 8) & 0xf));

  // disable all Frame Composer interrupts
  WriteReg(HDMITX_DWC_FC_MASK0, 0xe7);
  WriteReg(HDMITX_DWC_FC_MASK1, 0xfb);
  WriteReg(HDMITX_DWC_FC_MASK2, 0x3);

  // No pixel repetition for the currently supported resolution
  // TODO: pixel repetition is 0 for most progressive. We don't support interlaced
  static constexpr uint8_t kPixelRepeat = 0;
  WriteReg(HDMITX_DWC_FC_PRCONF, ((kPixelRepeat + 1) << 4) | (kPixelRepeat) << 0);

  // Skip HDCP for now

  // Clear Interrupts
  WriteReg(HDMITX_DWC_IH_FC_STAT0, 0xff);
  WriteReg(HDMITX_DWC_IH_FC_STAT1, 0xff);
  WriteReg(HDMITX_DWC_IH_FC_STAT2, 0xff);
  WriteReg(HDMITX_DWC_IH_AS_STAT0, 0xff);
  WriteReg(HDMITX_DWC_IH_PHY_STAT0, 0xff);
  WriteReg(HDMITX_DWC_IH_I2CM_STAT0, 0xff);
  WriteReg(HDMITX_DWC_IH_CEC_STAT0, 0xff);
  WriteReg(HDMITX_DWC_IH_VP_STAT0, 0xff);
  WriteReg(HDMITX_DWC_IH_I2CMPHY_STAT0, 0xff);
  WriteReg(HDMITX_DWC_A_APIINTCLR, 0xff);
  WriteReg(HDMITX_DWC_HDCP22REG_STAT, 0xff);
}

void HdmiTransmitterControllerImpl::SetupInterrupts() {
  // setup interrupts we care about
  WriteReg(HDMITX_DWC_IH_MUTE_FC_STAT0, 0xff);
  WriteReg(HDMITX_DWC_IH_MUTE_FC_STAT1, 0xff);
  WriteReg(HDMITX_DWC_IH_MUTE_FC_STAT2, 0x3);

  WriteReg(HDMITX_DWC_IH_MUTE_AS_STAT0, 0x7);  // mute all

  WriteReg(HDMITX_DWC_IH_MUTE_PHY_STAT0, 0x3f);

  WriteReg(HDMITX_DWC_IH_MUTE_I2CM_STAT0, 1 << 1);

  // turn all cec-related interrupts on
  WriteReg(HDMITX_DWC_IH_MUTE_CEC_STAT0, 0x0);

  WriteReg(HDMITX_DWC_IH_MUTE_VP_STAT0, 0xff);

  WriteReg(HDMITX_DWC_IH_MUTE_I2CMPHY_STAT0, 0x03);

  // enable global interrupt
  WriteReg(HDMITX_DWC_IH_MUTE, 0x0);
}

void HdmiTransmitterControllerImpl::Reset() {
  // reset
  WriteReg(HDMITX_DWC_MC_SWRSTZREQ, 0x00);
  usleep(10);
  WriteReg(HDMITX_DWC_MC_SWRSTZREQ, 0x7d);
  // why???
  WriteReg(HDMITX_DWC_FC_VSYNCINWIDTH, ReadReg(HDMITX_DWC_FC_VSYNCINWIDTH));

  WriteReg(HDMITX_DWC_MC_CLKDIS, 0);
}

void HdmiTransmitterControllerImpl::SetupScdc(bool is4k) {
  uint8_t scdc_data = 0;
  ScdcRead(0x1, &scdc_data);
  FDF_LOG(INFO, "version is %s\n", (scdc_data == 1) ? "2.0" : "<= 1.4");
  // scdc write is done twice in uboot
  // TODO: find scdc register def
  ScdcWrite(0x2, 0x1);
  ScdcWrite(0x2, 0x1);

  if (is4k) {
    ScdcWrite(0x20, 3);
    ScdcWrite(0x20, 3);
  } else {
    ScdcWrite(0x20, 0);
    ScdcWrite(0x20, 0);
  }
}

void HdmiTransmitterControllerImpl::ResetFc() {
  auto regval = ReadReg(HDMITX_DWC_FC_INVIDCONF);
  regval &= ~(1 << 3);  // clear hdmi mode select
  WriteReg(HDMITX_DWC_FC_INVIDCONF, regval);
  usleep(1);
  regval = ReadReg(HDMITX_DWC_FC_INVIDCONF);
  regval |= (1 << 3);  // clear hdmi mode select
  WriteReg(HDMITX_DWC_FC_INVIDCONF, regval);
  usleep(1);
}

void HdmiTransmitterControllerImpl::SetFcScramblerCtrl(bool is4k) {
  if (is4k) {
    // Set
    WriteReg(HDMITX_DWC_FC_SCRAMBLER_CTRL, ReadReg(HDMITX_DWC_FC_SCRAMBLER_CTRL) | (1 << 0));
  } else {
    // Clear
    WriteReg(HDMITX_DWC_FC_SCRAMBLER_CTRL, 0);
  }
}

void HdmiTransmitterControllerImpl::ConfigCsc(const ColorParam& color_param) {
  uint8_t csc_coef_a1_msb;
  uint8_t csc_coef_a1_lsb;
  uint8_t csc_coef_a2_msb;
  uint8_t csc_coef_a2_lsb;
  uint8_t csc_coef_a3_msb;
  uint8_t csc_coef_a3_lsb;
  uint8_t csc_coef_a4_msb;
  uint8_t csc_coef_a4_lsb;
  uint8_t csc_coef_b1_msb;
  uint8_t csc_coef_b1_lsb;
  uint8_t csc_coef_b2_msb;
  uint8_t csc_coef_b2_lsb;
  uint8_t csc_coef_b3_msb;
  uint8_t csc_coef_b3_lsb;
  uint8_t csc_coef_b4_msb;
  uint8_t csc_coef_b4_lsb;
  uint8_t csc_coef_c1_msb;
  uint8_t csc_coef_c1_lsb;
  uint8_t csc_coef_c2_msb;
  uint8_t csc_coef_c2_lsb;
  uint8_t csc_coef_c3_msb;
  uint8_t csc_coef_c3_lsb;
  uint8_t csc_coef_c4_msb;
  uint8_t csc_coef_c4_lsb;
  uint8_t csc_scale;

  // Color space conversion is needed by default.
  uint8_t main_controller_feed_through_control = MC_FLOWCTRL_ENB_CSC;
  if (color_param.input_color_format == color_param.output_color_format) {
    // no need to convert
    main_controller_feed_through_control = MC_FLOWCTRL_BYPASS_CSC;
  }
  WriteReg(HDMITX_DWC_MC_FLOWCTRL, main_controller_feed_through_control);

  // Since we don't support 422 at this point, set csc_cfg to 0
  WriteReg(HDMITX_DWC_CSC_CFG, 0);

  // Co-efficient values are from DesignWare Core HDMI TX Video Datapath Application Note V2.1

  // First determine whether we need to convert or not
  if (color_param.input_color_format != color_param.output_color_format) {
    if (color_param.input_color_format == ColorFormat::kCfRgb) {
      // from RGB
      csc_coef_a1_msb = 0x25;
      csc_coef_a1_lsb = 0x91;
      csc_coef_a2_msb = 0x13;
      csc_coef_a2_lsb = 0x23;
      csc_coef_a3_msb = 0x07;
      csc_coef_a3_lsb = 0x4C;
      csc_coef_a4_msb = 0x00;
      csc_coef_a4_lsb = 0x00;
      csc_coef_b1_msb = 0xE5;
      csc_coef_b1_lsb = 0x34;
      csc_coef_b2_msb = 0x20;
      csc_coef_b2_lsb = 0x00;
      csc_coef_b3_msb = 0xFA;
      csc_coef_b3_lsb = 0xCC;
      switch (color_param.color_depth) {
        case ColorDepth::kCd24B:
          csc_coef_b4_msb = 0x02;
          csc_coef_b4_lsb = 0x00;
          csc_coef_c4_msb = 0x02;
          csc_coef_c4_lsb = 0x00;
          break;
        case ColorDepth::kCd30B:
          csc_coef_b4_msb = 0x08;
          csc_coef_b4_lsb = 0x00;
          csc_coef_c4_msb = 0x08;
          csc_coef_c4_lsb = 0x00;
          break;
        case ColorDepth::kCd36B:
          csc_coef_b4_msb = 0x20;
          csc_coef_b4_lsb = 0x00;
          csc_coef_c4_msb = 0x20;
          csc_coef_c4_lsb = 0x00;
          break;
        default:
          csc_coef_b4_msb = 0x20;
          csc_coef_b4_lsb = 0x00;
          csc_coef_c4_msb = 0x20;
          csc_coef_c4_lsb = 0x00;
      }
      csc_coef_c1_msb = 0xEA;
      csc_coef_c1_lsb = 0xCD;
      csc_coef_c2_msb = 0xF5;
      csc_coef_c2_lsb = 0x33;
      csc_coef_c3_msb = 0x20;
      csc_coef_c3_lsb = 0x00;
      csc_scale = 0;
    } else {
      // to RGB
      csc_coef_a1_msb = 0x10;
      csc_coef_a1_lsb = 0x00;
      csc_coef_a2_msb = 0xf4;
      csc_coef_a2_lsb = 0x93;
      csc_coef_a3_msb = 0xfa;
      csc_coef_a3_lsb = 0x7f;
      csc_coef_b1_msb = 0x10;
      csc_coef_b1_lsb = 0x00;
      csc_coef_b2_msb = 0x16;
      csc_coef_b2_lsb = 0x6e;
      csc_coef_b3_msb = 0x00;
      csc_coef_b3_lsb = 0x00;
      switch (color_param.color_depth) {
        case ColorDepth::kCd24B:
          csc_coef_a4_msb = 0x00;
          csc_coef_a4_lsb = 0x87;
          csc_coef_b4_msb = 0xff;
          csc_coef_b4_lsb = 0x4d;
          csc_coef_c4_msb = 0xff;
          csc_coef_c4_lsb = 0x1e;
          break;
        case ColorDepth::kCd30B:
          csc_coef_a4_msb = 0x02;
          csc_coef_a4_lsb = 0x1d;
          csc_coef_b4_msb = 0xfd;
          csc_coef_b4_lsb = 0x33;
          csc_coef_c4_msb = 0xfc;
          csc_coef_c4_lsb = 0x75;
          break;
        case ColorDepth::kCd36B:
          csc_coef_a4_msb = 0x08;
          csc_coef_a4_lsb = 0x77;
          csc_coef_b4_msb = 0xf4;
          csc_coef_b4_lsb = 0xc9;
          csc_coef_c4_msb = 0xf1;
          csc_coef_c4_lsb = 0xd3;
          break;
        default:
          csc_coef_a4_msb = 0x08;
          csc_coef_a4_lsb = 0x77;
          csc_coef_b4_msb = 0xf4;
          csc_coef_b4_lsb = 0xc9;
          csc_coef_c4_msb = 0xf1;
          csc_coef_c4_lsb = 0xd3;
      }
      csc_coef_b4_msb = 0xff;
      csc_coef_b4_lsb = 0x4d;
      csc_coef_c1_msb = 0x10;
      csc_coef_c1_lsb = 0x00;
      csc_coef_c2_msb = 0x00;
      csc_coef_c2_lsb = 0x00;
      csc_coef_c3_msb = 0x1c;
      csc_coef_c3_lsb = 0x5a;
      csc_coef_c4_msb = 0xff;
      csc_coef_c4_lsb = 0x1e;
      csc_scale = 2;
    }
  } else {
    // No conversion. re-write default values just in case
    csc_coef_a1_msb = 0x20;
    csc_coef_a1_lsb = 0x00;
    csc_coef_a2_msb = 0x00;
    csc_coef_a2_lsb = 0x00;
    csc_coef_a3_msb = 0x00;
    csc_coef_a3_lsb = 0x00;
    csc_coef_a4_msb = 0x00;
    csc_coef_a4_lsb = 0x00;
    csc_coef_b1_msb = 0x00;
    csc_coef_b1_lsb = 0x00;
    csc_coef_b2_msb = 0x20;
    csc_coef_b2_lsb = 0x00;
    csc_coef_b3_msb = 0x00;
    csc_coef_b3_lsb = 0x00;
    csc_coef_b4_msb = 0x00;
    csc_coef_b4_lsb = 0x00;
    csc_coef_c1_msb = 0x00;
    csc_coef_c1_lsb = 0x00;
    csc_coef_c2_msb = 0x00;
    csc_coef_c2_lsb = 0x00;
    csc_coef_c3_msb = 0x20;
    csc_coef_c3_lsb = 0x00;
    csc_coef_c4_msb = 0x00;
    csc_coef_c4_lsb = 0x00;
    csc_scale = 1;
  }

  WriteReg(HDMITX_DWC_CSC_COEF_A1_MSB, csc_coef_a1_msb);
  WriteReg(HDMITX_DWC_CSC_COEF_A1_LSB, csc_coef_a1_lsb);
  WriteReg(HDMITX_DWC_CSC_COEF_A2_MSB, csc_coef_a2_msb);
  WriteReg(HDMITX_DWC_CSC_COEF_A2_LSB, csc_coef_a2_lsb);
  WriteReg(HDMITX_DWC_CSC_COEF_A3_MSB, csc_coef_a3_msb);
  WriteReg(HDMITX_DWC_CSC_COEF_A3_LSB, csc_coef_a3_lsb);
  WriteReg(HDMITX_DWC_CSC_COEF_A4_MSB, csc_coef_a4_msb);
  WriteReg(HDMITX_DWC_CSC_COEF_A4_LSB, csc_coef_a4_lsb);
  WriteReg(HDMITX_DWC_CSC_COEF_B1_MSB, csc_coef_b1_msb);
  WriteReg(HDMITX_DWC_CSC_COEF_B1_LSB, csc_coef_b1_lsb);
  WriteReg(HDMITX_DWC_CSC_COEF_B2_MSB, csc_coef_b2_msb);
  WriteReg(HDMITX_DWC_CSC_COEF_B2_LSB, csc_coef_b2_lsb);
  WriteReg(HDMITX_DWC_CSC_COEF_B3_MSB, csc_coef_b3_msb);
  WriteReg(HDMITX_DWC_CSC_COEF_B3_LSB, csc_coef_b3_lsb);
  WriteReg(HDMITX_DWC_CSC_COEF_B4_MSB, csc_coef_b4_msb);
  WriteReg(HDMITX_DWC_CSC_COEF_B4_LSB, csc_coef_b4_lsb);
  WriteReg(HDMITX_DWC_CSC_COEF_C1_MSB, csc_coef_c1_msb);
  WriteReg(HDMITX_DWC_CSC_COEF_C1_LSB, csc_coef_c1_lsb);
  WriteReg(HDMITX_DWC_CSC_COEF_C2_MSB, csc_coef_c2_msb);
  WriteReg(HDMITX_DWC_CSC_COEF_C2_LSB, csc_coef_c2_lsb);
  WriteReg(HDMITX_DWC_CSC_COEF_C3_MSB, csc_coef_c3_msb);
  WriteReg(HDMITX_DWC_CSC_COEF_C3_LSB, csc_coef_c3_lsb);
  WriteReg(HDMITX_DWC_CSC_COEF_C4_MSB, csc_coef_c4_msb);
  WriteReg(HDMITX_DWC_CSC_COEF_C4_LSB, csc_coef_c4_lsb);

  // The value of `color_param.color_depth` is >= 0 and <= 7. So
  // `CSC_SCALE_COLOR_DEPTH()` won't cause an integer overflow.
  //
  // The value of `csc_scale` is 0, 1, or 2. So `CSC_SCALE_CSCSCALE()` won't
  // cause an integer overflow.
  //
  // `CSC_SCALE_COLOR_DEPTH(color_param.color_depth)` only occupies the bits 4-6
  // and `CSC_SCALE_CSCSCALE(csc_scale)` only occupies the bits 0-1. Thus they
  // won't overlap in any bit in the bitwise or operation.
  const uint8_t color_space_conversion_config =
      static_cast<const uint8_t>(
          CSC_SCALE_COLOR_DEPTH(static_cast<uint8_t>(color_param.color_depth))) |
      static_cast<const uint8_t>(CSC_SCALE_CSCSCALE(csc_scale));
  WriteReg(HDMITX_DWC_CSC_SCALE, color_space_conversion_config);
}

zx_status_t HdmiTransmitterControllerImpl::EdidTransfer(const i2c_impl_op_t* op_list,
                                                        size_t op_count) {
  uint8_t segment_num = 0;
  uint8_t offset = 0;
  for (unsigned i = 0; i < op_count; i++) {
    auto op = op_list[i];

    // The HDMITX_DWC_I2CM registers are a limited interface to the i2c bus for the E-DDC
    // protocol, which is good enough for the bus this device provides.
    if (op.address == 0x30 && !op.is_read && op.data_size == 1) {
      segment_num = *((const uint8_t*)op.data_buffer);
    } else if (op.address == 0x50 && !op.is_read && op.data_size == 1) {
      offset = *((const uint8_t*)op.data_buffer);
    } else if (op.address == 0x50 && op.is_read) {
      if (op.data_size % 8 != 0) {
        return ZX_ERR_NOT_SUPPORTED;
      }

      WriteReg(HDMITX_DWC_I2CM_SLAVE, 0x50);
      WriteReg(HDMITX_DWC_I2CM_SEGADDR, 0x30);
      WriteReg(HDMITX_DWC_I2CM_SEGPTR, segment_num);

      for (uint32_t i = 0; i < op.data_size; i += 8) {
        WriteReg(HDMITX_DWC_I2CM_ADDRESS, offset);
        WriteReg(HDMITX_DWC_I2CM_OPERATION, 1 << 2);
        offset = static_cast<uint8_t>(offset + 8);

        uint32_t timeout = 0;
        while ((!(ReadReg(HDMITX_DWC_IH_I2CM_STAT0) & (1 << 1))) && (timeout < 5)) {
          usleep(1000);
          timeout++;
        }
        if (timeout == 5) {
          FDF_LOG(ERROR, "HDMI DDC TimeOut\n");
          return ZX_ERR_TIMED_OUT;
        }
        usleep(1000);
        WriteReg(HDMITX_DWC_IH_I2CM_STAT0, 1 << 1);  // clear INT

        for (int j = 0; j < 8; j++) {
          uint32_t address = static_cast<uint32_t>(HDMITX_DWC_I2CM_READ_BUFF0 + j);
          ((uint8_t*)op.data_buffer)[i + j] = static_cast<uint8_t>(ReadReg(address));
        }
      }
    } else {
      return ZX_ERR_NOT_SUPPORTED;
    }

    if (op.stop) {
      segment_num = 0;
      offset = 0;
    }
  }

  return ZX_OK;
}

#define PRINT_REG(name) PrintReg(#name, (name))
void HdmiTransmitterControllerImpl::PrintReg(const char* name, uint32_t address) {
  FDF_LOG(INFO, "%s (0x%4x): %u", name, address, ReadReg(address));
}

void HdmiTransmitterControllerImpl::PrintRegisters() {
  FDF_LOG(INFO, "------------HdmiDw Registers------------");

  PRINT_REG(HDMITX_DWC_A_APIINTCLR);
  PRINT_REG(HDMITX_DWC_CSC_CFG);
  PRINT_REG(HDMITX_DWC_CSC_COEF_A1_MSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_A1_LSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_A2_MSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_A2_LSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_A3_MSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_A3_LSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_A4_MSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_A4_LSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_B1_MSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_B1_LSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_B2_MSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_B2_LSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_B3_MSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_B3_LSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_B4_MSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_B4_LSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_C1_MSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_C1_LSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_C2_MSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_C2_LSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_C3_MSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_C3_LSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_C4_MSB);
  PRINT_REG(HDMITX_DWC_CSC_COEF_C4_LSB);
  PRINT_REG(HDMITX_DWC_CSC_SCALE);
  PRINT_REG(HDMITX_DWC_FC_ACTSPC_HDLR_CFG);
  PRINT_REG(HDMITX_DWC_FC_AVICONF0);
  PRINT_REG(HDMITX_DWC_FC_AVICONF1);
  PRINT_REG(HDMITX_DWC_FC_AVICONF2);
  PRINT_REG(HDMITX_DWC_FC_AVICONF3);
  PRINT_REG(HDMITX_DWC_FC_CTRLDUR);
  PRINT_REG(HDMITX_DWC_FC_EXCTRLDUR);
  PRINT_REG(HDMITX_DWC_FC_EXCTRLSPAC);
  PRINT_REG(HDMITX_DWC_FC_GCP);
  PRINT_REG(HDMITX_DWC_FC_HSYNCINDELAY0);
  PRINT_REG(HDMITX_DWC_FC_HSYNCINDELAY1);
  PRINT_REG(HDMITX_DWC_FC_HSYNCINWIDTH0);
  PRINT_REG(HDMITX_DWC_FC_HSYNCINWIDTH1);
  PRINT_REG(HDMITX_DWC_FC_INHACTV0);
  PRINT_REG(HDMITX_DWC_FC_INHACTV1);
  PRINT_REG(HDMITX_DWC_FC_INHBLANK0);
  PRINT_REG(HDMITX_DWC_FC_INHBLANK1);
  PRINT_REG(HDMITX_DWC_FC_INVACTV0);
  PRINT_REG(HDMITX_DWC_FC_INVACTV1);
  PRINT_REG(HDMITX_DWC_FC_INVACT_2D_0);
  PRINT_REG(HDMITX_DWC_FC_INVACT_2D_1);
  PRINT_REG(HDMITX_DWC_FC_INVBLANK);
  PRINT_REG(HDMITX_DWC_FC_INVIDCONF);
  PRINT_REG(HDMITX_DWC_FC_MASK0);
  PRINT_REG(HDMITX_DWC_FC_MASK1);
  PRINT_REG(HDMITX_DWC_FC_MASK2);
  PRINT_REG(HDMITX_DWC_FC_PRCONF);
  PRINT_REG(HDMITX_DWC_FC_SCRAMBLER_CTRL);
  PRINT_REG(HDMITX_DWC_FC_VSYNCINDELAY);
  PRINT_REG(HDMITX_DWC_FC_VSYNCINWIDTH);
  PRINT_REG(HDMITX_DWC_HDCP22REG_STAT);
  PRINT_REG(HDMITX_DWC_I2CM_CTLINT);
  PRINT_REG(HDMITX_DWC_I2CM_DIV);
  PRINT_REG(HDMITX_DWC_I2CM_FS_SCL_HCNT_1);
  PRINT_REG(HDMITX_DWC_I2CM_FS_SCL_HCNT_0);
  PRINT_REG(HDMITX_DWC_I2CM_FS_SCL_LCNT_1);
  PRINT_REG(HDMITX_DWC_I2CM_FS_SCL_LCNT_0);
  PRINT_REG(HDMITX_DWC_I2CM_INT);
  PRINT_REG(HDMITX_DWC_I2CM_SDA_HOLD);
  PRINT_REG(HDMITX_DWC_I2CM_SCDC_UPDATE);
  PRINT_REG(HDMITX_DWC_I2CM_SS_SCL_HCNT_1);
  PRINT_REG(HDMITX_DWC_I2CM_SS_SCL_HCNT_0);
  PRINT_REG(HDMITX_DWC_I2CM_SS_SCL_LCNT_1);
  PRINT_REG(HDMITX_DWC_I2CM_SS_SCL_LCNT_0);
  PRINT_REG(HDMITX_DWC_IH_AS_STAT0);
  PRINT_REG(HDMITX_DWC_IH_CEC_STAT0);
  PRINT_REG(HDMITX_DWC_IH_FC_STAT0);
  PRINT_REG(HDMITX_DWC_IH_FC_STAT1);
  PRINT_REG(HDMITX_DWC_IH_FC_STAT2);
  PRINT_REG(HDMITX_DWC_IH_I2CM_STAT0);
  PRINT_REG(HDMITX_DWC_IH_I2CMPHY_STAT0);
  PRINT_REG(HDMITX_DWC_IH_MUTE);
  PRINT_REG(HDMITX_DWC_IH_MUTE_AS_STAT0);
  PRINT_REG(HDMITX_DWC_IH_MUTE_CEC_STAT0);
  PRINT_REG(HDMITX_DWC_IH_MUTE_FC_STAT0);
  PRINT_REG(HDMITX_DWC_IH_MUTE_FC_STAT1);
  PRINT_REG(HDMITX_DWC_IH_MUTE_FC_STAT2);
  PRINT_REG(HDMITX_DWC_IH_MUTE_I2CM_STAT0);
  PRINT_REG(HDMITX_DWC_IH_MUTE_I2CMPHY_STAT0);
  PRINT_REG(HDMITX_DWC_IH_MUTE_PHY_STAT0);
  PRINT_REG(HDMITX_DWC_IH_MUTE_VP_STAT0);
  PRINT_REG(HDMITX_DWC_IH_PHY_STAT0);
  PRINT_REG(HDMITX_DWC_IH_VP_STAT0);
  PRINT_REG(HDMITX_DWC_MC_FLOWCTRL);
  PRINT_REG(HDMITX_DWC_MC_SWRSTZREQ);
  PRINT_REG(HDMITX_DWC_MC_CLKDIS);
  PRINT_REG(HDMITX_DWC_TX_INVID0);
  PRINT_REG(HDMITX_DWC_TX_INSTUFFING);
  PRINT_REG(HDMITX_DWC_TX_GYDATA0);
  PRINT_REG(HDMITX_DWC_TX_GYDATA1);
  PRINT_REG(HDMITX_DWC_TX_RCRDATA0);
  PRINT_REG(HDMITX_DWC_TX_RCRDATA1);
  PRINT_REG(HDMITX_DWC_TX_BCBDATA0);
  PRINT_REG(HDMITX_DWC_TX_BCBDATA1);
  PRINT_REG(HDMITX_DWC_VP_CONF);
  PRINT_REG(HDMITX_DWC_VP_MASK);
  PRINT_REG(HDMITX_DWC_VP_PR_CD);
  PRINT_REG(HDMITX_DWC_VP_REMAP);
  PRINT_REG(HDMITX_DWC_VP_STUFF);
}
#undef PRINT_REG

}  // namespace designware_hdmi
