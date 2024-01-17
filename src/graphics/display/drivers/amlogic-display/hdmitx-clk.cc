// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/amlogic-display.h"
#include "src/graphics/display/drivers/amlogic-display/clock-regs.h"
#include "src/graphics/display/drivers/amlogic-display/fixed-point-util.h"
#include "src/graphics/display/drivers/amlogic-display/hdmi-host.h"
#include "src/graphics/display/drivers/amlogic-display/hhi-regs.h"
#include "src/graphics/display/drivers/amlogic-display/vpu-regs.h"

namespace amlogic_display {

// TODO(https://fxbug.dev/69072): Reconcile with amlogic-clock

namespace {

const uint32_t kFracMax = 131072;

}  // namespace

void HdmiHost::WaitForPllLocked() {
  bool err = false;
  do {
    unsigned int st = 0;
    int cnt = 10000;
    while (cnt--) {
      usleep(5);
      auto reg = HhiHdmiPllCntlReg::Get().ReadFrom(&hhi_mmio_);
      st = (reg.hdmi_dpll_lock() == 1) && (reg.hdmi_dpll_lock_a() == 1);
      if (st) {
        err = false;
        break;
      } else { /* reset hpll */
        HhiHdmiPllCntlReg::Get().ReadFrom(&hhi_mmio_).set_hdmi_dpll_reset(1).WriteTo(&hhi_mmio_);
        HhiHdmiPllCntlReg::Get().ReadFrom(&hhi_mmio_).set_hdmi_dpll_reset(0).WriteTo(&hhi_mmio_);
      }
    }
    zxlogf(ERROR, "pll[0x%x] reset %d times", HHI_HDMI_PLL_CNTL0, 10000 - cnt);
    if (cnt <= 0)
      err = true;
  } while (err);
}

namespace {

VideoInputUnitEncoderMuxControl::Encoder EncoderSelectionFromViuType(viu_type type) {
  switch (type) {
    case VIU_ENCL:
      return VideoInputUnitEncoderMuxControl::Encoder::kLcd;
    case VIU_ENCI:
      return VideoInputUnitEncoderMuxControl::Encoder::kInterlaced;
    case VIU_ENCP:
      return VideoInputUnitEncoderMuxControl::Encoder::kProgressive;
    case VIU_ENCT:
      return VideoInputUnitEncoderMuxControl::Encoder::kTvPanel;
  }
  zxlogf(ERROR, "Incorrect VIU type: %u", type);
  return VideoInputUnitEncoderMuxControl::Encoder::kLcd;
}

}  // namespace

void HdmiHost::ConfigurePll(const pll_param& pll_params) {
  // Set VIU Mux Ctrl
  if (pll_params.viu_channel == 1) {
    VideoInputUnitEncoderMuxControl::Get()
        .ReadFrom(&vpu_mmio_)
        .set_vsync_shared_by_viu_blocks(false)
        .set_viu1_encoder_selection(
            EncoderSelectionFromViuType(static_cast<viu_type>(pll_params.viu_type)))
        .WriteTo(&vpu_mmio_);
  } else {
    VideoInputUnitEncoderMuxControl::Get()
        .ReadFrom(&vpu_mmio_)
        .set_vsync_shared_by_viu_blocks(false)
        .set_viu2_encoder_selection(
            EncoderSelectionFromViuType(static_cast<viu_type>(pll_params.viu_type)))
        .WriteTo(&vpu_mmio_);
  }
  HdmiClockControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_hdmi_tx_system_clock_selection(
          HdmiClockControl::HdmiTxSystemClockSource::kExternalOscillator24Mhz)
      .SetHdmiTxSystemClockDivider(1)
      .set_hdmi_tx_system_clock_enabled(true)
      .WriteTo(&hhi_mmio_);

  ConfigureHpllClkOut(pll_params.hpll_clk_out);

  HhiHdmiPllCntlReg::Get()
      .ReadFrom(&hhi_mmio_)
      .set_hdmi_dpll_od1(pll_params.od1 >> 1)
      .set_hdmi_dpll_od2(pll_params.od2 >> 1)
      .set_hdmi_dpll_od3(pll_params.od3 >> 1)
      .WriteTo(&hhi_mmio_);

  ConfigureHdmiClockTree(pll_params.vid_pll_divider_ratio);

  VideoClock1Control::Get()
      .ReadFrom(&hhi_mmio_)
      .set_mux_source(VideoClockMuxSource::kVideoPll)
      .WriteTo(&hhi_mmio_);
  VideoClock1Divider::Get()
      .ReadFrom(&hhi_mmio_)
      .SetDivider0((pll_params.vid_clk_div == 0) ? 1 : (pll_params.vid_clk_div))
      .WriteTo(&hhi_mmio_);
  VideoClock1Control::Get()
      .ReadFrom(&hhi_mmio_)
      .set_div4_enabled(true)
      .set_div2_enabled(true)
      .set_div1_enabled(true)
      .WriteTo(&hhi_mmio_);

  HdmiClockControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_hdmi_tx_pixel_clock_selection(HdmiClockControl::HdmiTxPixelClockSource::kVideoClock1)
      .WriteTo(&hhi_mmio_);

  VideoClockOutputControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_hdmi_tx_pixel_clock_enabled(true)
      .WriteTo(&hhi_mmio_);

  if (pll_params.encp_div != (uint32_t)-1) {
    VideoClock1Divider::Get()
        .ReadFrom(&hhi_mmio_)
        .set_encp_clock_selection(EncoderClockSource::kVideoClock1)
        .WriteTo(&hhi_mmio_);
    VideoClockOutputControl::Get()
        .ReadFrom(&hhi_mmio_)
        .set_encoder_progressive_enabled(true)
        .WriteTo(&hhi_mmio_);
    VideoClock1Control::Get().ReadFrom(&hhi_mmio_).set_divider0_enabled(true).WriteTo(&hhi_mmio_);
  }
  if (pll_params.enci_div != (uint32_t)-1) {
    VideoClock1Divider::Get()
        .ReadFrom(&hhi_mmio_)
        .set_enci_clock_selection(EncoderClockSource::kVideoClock1)
        .WriteTo(&hhi_mmio_);
    VideoClockOutputControl::Get()
        .ReadFrom(&hhi_mmio_)
        .set_encoder_interlaced_enabled(true)
        .WriteTo(&hhi_mmio_);
    VideoClock1Control::Get().ReadFrom(&hhi_mmio_).set_divider0_enabled(true).WriteTo(&hhi_mmio_);
  }
}

void HdmiHost::ConfigureHpllClkOut(uint32_t hpll) {
  float desired_pll = (float)hpll / (float)24000;
  uint8_t whole;
  uint16_t frac;
  whole = (uint8_t)desired_pll;
  frac = static_cast<uint16_t>(((float)desired_pll - (float)whole) * kFracMax);

  zxlogf(ERROR, "Desired PLL = %f (frac = %d, whole = %d) (hpll = %d)", desired_pll, frac, whole,
         hpll);

  HhiHdmiPllCntlReg::Get().FromValue(0x0b3a0400).set_hdmi_dpll_M(whole).WriteTo(&hhi_mmio_);

  /* Enable and reset */
  HhiHdmiPllCntlReg::Get()
      .ReadFrom(&hhi_mmio_)
      .set_hdmi_dpll_en(1)
      .set_hdmi_dpll_reset(1)
      .WriteTo(&hhi_mmio_);

  HhiHdmiPllCntl1Reg::Get().FromValue(frac).WriteTo(&hhi_mmio_);
  HhiHdmiPllCntl2Reg::Get().FromValue(0x0).WriteTo(&hhi_mmio_);

  /* G12A HDMI PLL Needs specific parameters for 5.4GHz */
  if (whole >= 0xf7) {
    HhiHdmiPllCntl3Reg::Get().FromValue(0x6a685c00).WriteTo(&hhi_mmio_);
    HhiHdmiPllCntl4Reg::Get().FromValue(0x11551293).WriteTo(&hhi_mmio_);
    HhiHdmiPllCntl5Reg::Get().FromValue(0x39272000).WriteTo(&hhi_mmio_);
    HhiHdmiPllStsReg::Get().FromValue(0x55540000).WriteTo(&hhi_mmio_);
  } else {
    HhiHdmiPllCntl3Reg::Get().FromValue(0x0a691c00).WriteTo(&hhi_mmio_);
    HhiHdmiPllCntl4Reg::Get().FromValue(0x33771290).WriteTo(&hhi_mmio_);
    HhiHdmiPllCntl5Reg::Get().FromValue(0x39272000).WriteTo(&hhi_mmio_);
    HhiHdmiPllStsReg::Get().FromValue(0x50540000).WriteTo(&hhi_mmio_);
  }

  /* Reset PLL */
  HhiHdmiPllCntlReg::Get().ReadFrom(&hhi_mmio_).set_hdmi_dpll_reset(1).WriteTo(&hhi_mmio_);

  /* UN-Reset PLL */
  HhiHdmiPllCntlReg::Get().ReadFrom(&hhi_mmio_).set_hdmi_dpll_reset(0).WriteTo(&hhi_mmio_);

  /* Poll for lock bits */
  WaitForPllLocked();
}

void HdmiHost::ConfigureHdmiClockTree(int divider_ratio) {
  ZX_DEBUG_ASSERT_MSG(std::find(HdmiClockTreeControl::kSupportedFrequencyDividerRatios.begin(),
                                HdmiClockTreeControl::kSupportedFrequencyDividerRatios.end(),
                                ToU28_4(divider_ratio)) !=
                          HdmiClockTreeControl::kSupportedFrequencyDividerRatios.end(),
                      "HDMI clock tree divider ratio %d is not supported.", divider_ratio);

  // TODO(https://fxbug.dev/42086073): When the divider ratio is 6.25, some
  // Amlogic-provided code triggers a software reset of the `vid_pll_div` clock
  // before setting the HDMI clock tree, while some other Amlogic-provided code
  // doesn't do any reset.
  //
  // Currently fractional divider ratios are not supported; this needs to
  // be addressed once we add fraction divider ratio support.

  HdmiClockTreeControl hdmi_clock_tree_control = HdmiClockTreeControl::Get().ReadFrom(&hhi_mmio_);
  hdmi_clock_tree_control.set_clock_output_enabled(false).WriteTo(&hhi_mmio_);

  // This implementation deviates from the Amlogic-provided code.
  //
  // The Amlogic-provided code changes the pattern generator enablement, mode
  // selection and the state in different register writes, while the current
  // implementation changes all of them at the same time. Experiments on Khadas
  // VIM3 (A311D) show that our implementation works correctly.
  hdmi_clock_tree_control.ReadFrom(&hhi_mmio_)
      .SetFrequencyDividerRatio(ToU28_4(divider_ratio))
      .set_preset_pattern_update_enabled(true)
      .WriteTo(&hhi_mmio_);

  hdmi_clock_tree_control.ReadFrom(&hhi_mmio_)
      .set_preset_pattern_update_enabled(false)
      .WriteTo(&hhi_mmio_);

  hdmi_clock_tree_control.ReadFrom(&hhi_mmio_).set_clock_output_enabled(true).WriteTo(&hhi_mmio_);
}

}  // namespace amlogic_display
