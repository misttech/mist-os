// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/hdmi-host.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/ddk/debug.h>

#include <limits>

#include "src/graphics/display/drivers/amlogic-display/clock-regs.h"
#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/gpio-mux-regs.h"
#include "src/graphics/display/drivers/amlogic-display/hhi-regs.h"
#include "src/graphics/display/drivers/amlogic-display/power-regs.h"
#include "src/graphics/display/drivers/amlogic-display/vpu-regs.h"

namespace amlogic_display {

namespace {

struct reg_val_pair {
  uint32_t reg;
  uint32_t val;
};

static const struct reg_val_pair ENC_LUT_GEN[] = {
    {VPU_ENCP_VIDEO_EN, 0},           {VPU_ENCI_VIDEO_EN, 0},
    {VPU_ENCP_VIDEO_MODE, 0x4040},    {VPU_ENCP_VIDEO_MODE_ADV, 0x18},
    {VPU_VPU_VIU_VENC_MUX_CTRL, 0xA}, {VPU_ENCP_VIDEO_VSO_BEGIN, 16},
    {VPU_ENCP_VIDEO_VSO_END, 32},     {VPU_ENCI_VIDEO_EN, 0},
    {VPU_ENCP_VIDEO_EN, 1},           {0xFFFFFFFF, 0},
};

void TranslateDisplayMode(fidl::AnyArena& allocator, const display_mode_t& in_mode,
                          const ColorParam& in_color, DisplayMode* out_mode) {
  // Serves to translate between banjo struct display_mode_t and fidl struct DisplayMode
  fuchsia_hardware_hdmi::wire::StandardDisplayMode mode{
      .pixel_clock_10khz = in_mode.pixel_clock_10khz,
      .h_addressable = in_mode.h_addressable,
      .h_front_porch = in_mode.h_front_porch,
      .h_sync_pulse = in_mode.h_sync_pulse,
      .h_blanking = in_mode.h_blanking,
      .v_addressable = in_mode.v_addressable,
      .v_front_porch = in_mode.v_front_porch,
      .v_sync_pulse = in_mode.v_sync_pulse,
      .v_blanking = in_mode.v_blanking,
      .flags = in_mode.flags,
  };
  out_mode->set_mode(allocator, mode);

  ColorParam color{
      .input_color_format = in_color.input_color_format,
      .output_color_format = in_color.output_color_format,
      .color_depth = in_color.color_depth,
  };
  out_mode->set_color(color);
}

}  // namespace

zx_status_t HdmiHost::Init() {
  auto status = pdev_.MapMmio(MMIO_VPU, &vpu_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not map VPU mmio: %s", zx_status_get_string(status));
    return status;
  }

  status = pdev_.MapMmio(MMIO_HHI, &hhi_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not map HHI mmio: %s", zx_status_get_string(status));
    return status;
  }

  status = pdev_.MapMmio(MMIO_GPIO_MUX, &gpio_mux_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not map GPIO MUX mmio: %s", zx_status_get_string(status));
    return status;
  }

  auto res = hdmi_->PowerUp(1);  // only supports 1 display for now.
  if ((res.status() != ZX_OK) || res->is_error()) {
    zxlogf(ERROR, "Power Up failed: %s", res.FormatDescription().c_str());
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

zx_status_t HdmiHost::HostOn() {
  /* Step 1: Initialize various clocks related to the HDMI Interface*/
  SET_BIT32(GPIO_MUX, PAD_PULL_UP_EN_REG3, 0, 0, 2);
  SET_BIT32(GPIO_MUX, PAD_PULL_UP_REG3, 0, 0, 2);
  SET_BIT32(GPIO_MUX, P_PREG_PAD_GPIO3_EN_N, 3, 0, 2);
  SET_BIT32(GPIO_MUX, PERIPHS_PIN_MUX_B, 0x11, 0, 8);

  // enable clocks
  HdmiClockControl::Get()
      .ReadFrom(&(*hhi_mmio_))
      .SetHdmiTxSystemClockDivider(1)
      .set_hdmi_tx_system_clock_enabled(true)
      .set_hdmi_tx_system_clock_selection(
          HdmiClockControl::HdmiTxSystemClockSource::kExternalOscillator24Mhz)
      .WriteTo(&(*hhi_mmio_));

  // enable clk81 (needed for HDMI module and a bunch of other modules)
  HhiGclkMpeg2Reg::Get().ReadFrom(&(*hhi_mmio_)).set_clk81_en(1).WriteTo(&(*hhi_mmio_));

  // TODO(fxbug.com/132123): HDMI memory was supposed to be powered on during
  // the VPU power sequence. The AMLogic-supplied bringup code pauses for 5us
  // between each bit flip.
  auto memory_power0 = MemoryPower0::Get().ReadFrom(&hhi_mmio_.value());
  memory_power0.set_hdmi_memory0_powered_off(false);
  memory_power0.set_hdmi_memory1_powered_off(false);
  memory_power0.set_hdmi_memory2_powered_off(false);
  memory_power0.set_hdmi_memory3_powered_off(false);
  memory_power0.set_hdmi_memory4_powered_off(false);
  memory_power0.set_hdmi_memory5_powered_off(false);
  memory_power0.set_hdmi_memory6_powered_off(false);
  memory_power0.set_hdmi_memory7_powered_off(false);
  memory_power0.WriteTo(&hhi_mmio_.value());

  auto res = hdmi_->Reset(1);  // only supports 1 display for now
  if ((res.status() != ZX_OK) || res->is_error()) {
    zxlogf(ERROR, "Reset failed");
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

void HdmiHost::HostOff() {
  /* Close HDMITX PHY */
  WRITE32_REG(HHI, HHI_HDMI_PHY_CNTL0, 0);
  WRITE32_REG(HHI, HHI_HDMI_PHY_CNTL3, 0);
  /* Disable HPLL */
  WRITE32_REG(HHI, HHI_HDMI_PLL_CNTL0, 0);

  auto res = hdmi_->PowerDown(1);  // only supports 1 display for now
  if (res.status() != ZX_OK) {
    zxlogf(ERROR, "Power Down failed");
  }
}

zx_status_t HdmiHost::ModeSet(const display_mode_t& mode) {
  for (size_t i = 0; ENC_LUT_GEN[i].reg != 0xFFFFFFFF; i++) {
    WRITE32_REG(VPU, ENC_LUT_GEN[i].reg, ENC_LUT_GEN[i].val);
  }

  WRITE32_REG(
      VPU, VPU_ENCP_VIDEO_MAX_PXCNT,
      (p_.timings.venc_pixel_repeat) ? ((p_.timings.htotal << 1) - 1) : (p_.timings.htotal - 1));
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_MAX_LNCNT, p_.timings.vtotal - 1);

  if (p_.timings.venc_pixel_repeat) {
    SET_BIT32(VPU, VPU_ENCP_VIDEO_MODE_ADV, 1, 0, 1);
  }

  // Configure Encoder with detailed timing info (based on resolution)
  ConfigEncoder();

  // Configure VDAC
  WRITE32_REG(HHI, HHI_VDAC_CNTL0_G12A, 0);
  WRITE32_REG(HHI, HHI_VDAC_CNTL1_G12A, 8);  // set Cdac_pwd [whatever that is]

  fidl::Arena<2048> allocator;
  DisplayMode translated_mode(allocator);
  TranslateDisplayMode(allocator, mode, color_, &translated_mode);
  auto res = hdmi_->ModeSet(1, translated_mode);  // only supports 1 display for now
  if ((res.status() != ZX_OK) || res->is_error()) {
    zxlogf(ERROR, "Unable to initialize interface");
    return ZX_ERR_INTERNAL;
  }

  // Setup HDMI related registers in VPU
  // not really needed since we are not converting from 420/422. but set anyways
  VpuHdmiFmtCtrlReg::Get()
      .FromValue(0)
      .set_cntl_chroma_dnsmp(2)
      .set_cntl_hdmi_dith_en(0)
      .set_rounding_enable(1)
      .WriteTo(&(*vpu_mmio_));

  // setup some magic registers
  VpuHdmiDithCntlReg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_cntl_hdmi_dith_en(1)
      .set_hsync_invert(0)
      .set_vsync_invert(0)
      .WriteTo(&(*vpu_mmio_));

  // reset vpu bridge
  uint32_t wr_rate = VpuHdmiSettingReg::Get().ReadFrom(&(*vpu_mmio_)).wr_rate();
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_EN, 0);
  VpuHdmiSettingReg::Get()
      .ReadFrom(&(*vpu_mmio_))
      .set_src_sel(0)
      .set_wr_rate(0)
      .WriteTo(&(*vpu_mmio_));
  usleep(1);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_EN, 1);
  usleep(1);
  VpuHdmiSettingReg::Get().ReadFrom(&(*vpu_mmio_)).set_wr_rate(wr_rate).WriteTo(&(*vpu_mmio_));
  usleep(1);
  VpuHdmiSettingReg::Get().ReadFrom(&(*vpu_mmio_)).set_src_sel(2).WriteTo(&(*vpu_mmio_));

  // setup hdmi phy
  ConfigPhy();

  zxlogf(INFO, "done!!");
  return ZX_OK;
}

zx_status_t HdmiHost::EdidTransfer(const i2c_impl_op_t* op_list, size_t op_count) {
  auto ops = std::make_unique<EdidOp[]>(op_count);
  auto writes = std::make_unique<fidl::VectorView<uint8_t>[]>(op_count);
  auto reads = std::make_unique<uint16_t[]>(op_count);
  size_t write_cnt = 0;
  size_t read_cnt = 0;
  for (size_t i = 0; i < op_count; ++i) {
    ops[i].address = op_list[i].address;
    ops[i].is_write = !op_list[i].is_read;
    if (op_list[i].is_read) {
      if (op_list[i].data_size > std::numeric_limits<uint16_t>::max()) {
        return ZX_ERR_INVALID_ARGS;
      }
      reads[read_cnt] = static_cast<uint16_t>(op_list[i].data_size);
      read_cnt++;
    } else {
      writes[write_cnt] = fidl::VectorView<uint8_t>::FromExternal(
          const_cast<uint8_t*>(op_list[i].data_buffer), op_list[i].data_size);
      write_cnt++;
    }
  }
  auto all_ops = fidl::VectorView<EdidOp>::FromExternal(ops.get(), op_count);
  auto all_writes =
      fidl::VectorView<fidl::VectorView<uint8_t>>::FromExternal(writes.get(), write_cnt);
  auto all_reads = fidl::VectorView<uint16_t>::FromExternal(reads.get(), read_cnt);

  auto res = hdmi_->EdidTransfer(all_ops, all_writes, all_reads);
  if ((res.status() != ZX_OK) || res->is_error()) {
    zxlogf(ERROR, "Unable to perform Edid Transfer");
    return ZX_ERR_INTERNAL;
  }

  auto read = res->value()->read_segments_data;
  read_cnt = 0;
  for (size_t i = 0; i < op_count; ++i) {
    if (!op_list[i].is_read) {
      continue;
    }
    memcpy(op_list[i].data_buffer, read[read_cnt].data(), read[read_cnt].count());
    read_cnt++;
  }

  return ZX_OK;
}

namespace {

// Returns true iff the display PLL and clock trees can be programmed to
// generate a pixel clock of `pixel_clock_khz` kHz.
bool IsPixelClockSupported(int pixel_clock_khz) {
  // The minimum valid HDMI PLL VCO frequency doesn't match the frequency
  // specified in the Amlogic datasheets (3 GHz). However, experiments on
  // Khadas VIM3 (Amlogic A311D) shows that 2.9 GHz is a valid VCO frequency
  // and has fewer display glitches than using 5.8 GHz.
  constexpr int kMinimumValidHdmiPllVcoFrequencyKhz = 2'900'000;
  constexpr int kMaximumValidHdmiPllVcoFrequencyKhz = 6'000'000;

  // Fixed divisor values.
  //
  // HDMI clock tree divisor `vid_pll_div` == 5,
  // Video tree divisor /N0 `vid_clk_div` == 2,
  // Video tree ENCP clock selector `encp_div` == 1.
  //
  // TODO(fxbug.dev/133175): Factor this out for pixel clock checking and
  // calculation logics.
  constexpr int kFixedPllDivisionFactor = 5 * 2 * 1;

  // TODO(fxbug.dev/133175): Factor out ranges for each output frequency
  // divider so that they can be used for both clock checking and calculation.
  // OD1 = OD2 = OD3 = 1.
  constexpr int kMinimumPllDivisionFactor = 1 * 1 * 1;
  // OD1 = OD2 = OD3 = 4.
  constexpr int kMaximumPllDivisionFactor = 4 * 4 * 4;

  // The adjustable dividers OD1 / OD2 / OD3 cannot be calculated if the output
  // frequency using `kMinimumPllDivisionFactor` still exceeds the maximum
  // allowed value.
  constexpr int kMaximumAllowedPixelClockKhz =
      kMaximumValidHdmiPllVcoFrequencyKhz / (kFixedPllDivisionFactor * kMinimumPllDivisionFactor);
  if (pixel_clock_khz > kMaximumAllowedPixelClockKhz) {
    return false;
  }

  // The adjustable dividers OD1 / OD2 / OD3 cannot be calculated if the output
  // frequency using `kMaximumPllDivisionFactor` is still less than the minimum
  // allowed value.

  // ceil(kMinimumValidHdmiPllVcoFrequencyKhz / (kFixedPllDivisionFactor *
  // kMaximumPllDivisionFactor))
  constexpr int kMinimumAllowedPixelClockKhz =
      (kMinimumValidHdmiPllVcoFrequencyKhz + kFixedPllDivisionFactor * kMaximumPllDivisionFactor -
       1) /
      (kFixedPllDivisionFactor * kMaximumPllDivisionFactor);
  if (pixel_clock_khz < kMinimumAllowedPixelClockKhz) {
    return false;
  }

  return true;
}

}  // namespace

bool HdmiHost::IsDisplayModeSupported(const display_mode_t& mode) const {
  // TODO(fxbug.dev/133248): Interlaced modes are not supported.
  if (mode.flags & MODE_FLAG_INTERLACED) {
    return false;
  }

  const int pixel_clock_khz = mode.pixel_clock_10khz * 10;
  if (!IsPixelClockSupported(pixel_clock_khz)) {
    return false;
  }

  return true;
}

zx_status_t HdmiHost::GetVic(display_mode_t* disp_timing) { return GetVic(disp_timing, &p_); }

zx_status_t HdmiHost::GetVic(display_mode_t* disp_timing, hdmi_param* p) {
  ZX_DEBUG_ASSERT(disp_timing != nullptr);

  if (!IsDisplayModeSupported(*disp_timing)) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (disp_timing->v_addressable == 2160) {
    zxlogf(INFO, "4K Monitor Detected.");

    if ((disp_timing->pixel_clock_10khz * 10) == 533250) {
      // FIXME: 4K with reduced blanking (533.25MHz) does not work
      zxlogf(INFO, "4K @ 30Hz");
      disp_timing->flags &= ~MODE_FLAG_INTERLACED;
      disp_timing->pixel_clock_10khz = 29700;
      disp_timing->h_addressable = 3840;
      disp_timing->h_blanking = 560;
      disp_timing->h_front_porch = 176;
      disp_timing->h_sync_pulse = 88;
      disp_timing->flags |= MODE_FLAG_HSYNC_POSITIVE;
      disp_timing->v_addressable = 2160;
      disp_timing->v_blanking = 90;
      disp_timing->v_front_porch = 8;
      disp_timing->v_sync_pulse = 10;
      disp_timing->flags |= MODE_FLAG_VSYNC_POSITIVE;
    }
  }

  // Monitor has its own preferred timings. Use that
  p->timings.interlace_mode = disp_timing->flags & MODE_FLAG_INTERLACED;
  p->timings.pfreq = (disp_timing->pixel_clock_10khz * 10);  // KHz
  // TODO: pixel repetition is 0 for most progressive. We don't support interlaced
  p->timings.pixel_repeat = 0;
  p->timings.hactive = disp_timing->h_addressable;
  p->timings.hblank = disp_timing->h_blanking;
  p->timings.hfront = disp_timing->h_front_porch;
  p->timings.hsync = disp_timing->h_sync_pulse;
  p->timings.htotal = (p->timings.hactive) + (p->timings.hblank);
  p->timings.hback = (p->timings.hblank) - (p->timings.hfront + p->timings.hsync);
  p->timings.hpol = disp_timing->flags & MODE_FLAG_HSYNC_POSITIVE;

  p->timings.vactive = disp_timing->v_addressable;
  p->timings.vblank0 = disp_timing->v_blanking;
  p->timings.vfront = disp_timing->v_front_porch;
  p->timings.vsync = disp_timing->v_sync_pulse;
  p->timings.vtotal = (p->timings.vactive) + (p->timings.vblank0);
  p->timings.vback = (p->timings.vblank0) - (p->timings.vfront + p->timings.vsync);
  p->timings.vpol = disp_timing->flags & MODE_FLAG_VSYNC_POSITIVE;

  // FIXE: VENC Repeat is undocumented. It seems to be only needed for the following
  // resolutions: 1280x720p60, 1280x720p50, 720x480p60, 720x480i60, 720x576p50, 720x576i50
  // For now, we will simply not support this feature.
  p->timings.venc_pixel_repeat = 0;

  if (p->timings.pfreq > 500000) {
    p->phy_mode = 1;
  } else if (p->timings.pfreq > 200000) {
    p->phy_mode = 2;
  } else if (p->timings.pfreq > 100000) {
    p->phy_mode = 3;
  } else {
    p->phy_mode = 4;
  }

  // TODO: We probably need a more sophisticated method for calculating
  // clocks. This will do for now.
  p->pll_p_24b.viu_channel = 1;
  p->pll_p_24b.viu_type = VIU_ENCP;
  p->pll_p_24b.vid_pll_div = VID_PLL_DIV_5;
  p->pll_p_24b.vid_clk_div = 2;
  p->pll_p_24b.hdmi_tx_pixel_div = 1;
  p->pll_p_24b.encp_div = 1;
  p->pll_p_24b.od1 = 1;
  p->pll_p_24b.od2 = 1;
  p->pll_p_24b.od3 = 1;

  p->pll_p_24b.hpll_clk_out = (p->timings.pfreq * 10);
  while (p->pll_p_24b.hpll_clk_out < 2900000) {
    if (p->pll_p_24b.od1 < 4) {
      p->pll_p_24b.od1 *= 2;
      p->pll_p_24b.hpll_clk_out *= 2;
    } else if (p->pll_p_24b.od2 < 4) {
      p->pll_p_24b.od2 *= 2;
      p->pll_p_24b.hpll_clk_out *= 2;
    } else if (p->pll_p_24b.od3 < 4) {
      p->pll_p_24b.od3 *= 2;
      p->pll_p_24b.hpll_clk_out *= 2;
    } else {
      ZX_DEBUG_ASSERT_MSG(false,
                          "Failed to set HDMI PLL to a valid VCO frequency range for pixel clock "
                          "%d kHz. This should never happen since IsDisplayModeSupported() "
                          "returned true.",
                          p->timings.pfreq);
    }
  }
  ZX_DEBUG_ASSERT_MSG(p->pll_p_24b.hpll_clk_out <= 6000000,
                      "Calculated HDMI PLL VCO frequency (%" PRIu32
                      " kHz) exceeds the VCO frequency limit 6GHz. This should never happen since "
                      "IsDisplayModeSupported() returned true.",
                      p->pll_p_24b.hpll_clk_out);

  return ZX_OK;
}

void HdmiHost::ConfigEncoder() {
  uint32_t h_begin, h_end;
  uint32_t v_begin, v_end;
  uint32_t hs_begin, hs_end;
  uint32_t vs_begin, vs_end;
  uint32_t vsync_adjust = 0;
  uint32_t active_lines, total_lines;
  uint32_t venc_total_pixels, venc_active_pixels, venc_fp, venc_hsync;
  auto* p = &p_;

  active_lines = (p->timings.vactive / (1 + p->timings.interlace_mode));
  total_lines = (active_lines + p->timings.vblank0) +
                ((active_lines + p->timings.vblank1) * p->timings.interlace_mode);

  venc_total_pixels =
      (p->timings.htotal / (p->timings.pixel_repeat + 1)) * (p->timings.venc_pixel_repeat + 1);

  venc_active_pixels =
      (p->timings.hactive / (p->timings.pixel_repeat + 1)) * (p->timings.venc_pixel_repeat + 1);

  venc_fp =
      (p->timings.hfront / (p->timings.pixel_repeat + 1)) * (p->timings.venc_pixel_repeat + 1);

  venc_hsync =
      (p->timings.hsync / (p->timings.pixel_repeat + 1)) * (p->timings.venc_pixel_repeat + 1);

  SET_BIT32(VPU, VPU_ENCP_VIDEO_MODE, 1, 14, 1);  // DE Signal polarity
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_HAVON_BEGIN, p->timings.hsync + p->timings.hback);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_HAVON_END,
              p->timings.hsync + p->timings.hback + p->timings.hactive - 1);

  WRITE32_REG(VPU, VPU_ENCP_VIDEO_VAVON_BLINE, p->timings.vsync + p->timings.vback);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_VAVON_ELINE,
              p->timings.vsync + p->timings.vback + p->timings.vactive - 1);

  WRITE32_REG(VPU, VPU_ENCP_VIDEO_HSO_BEGIN, 0);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_HSO_END, p->timings.hsync);

  WRITE32_REG(VPU, VPU_ENCP_VIDEO_VSO_BLINE, 0);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_VSO_ELINE, p->timings.vsync);

  // Below calculations assume no pixel repeat and progressive mode.
  // HActive Start/End
  h_begin = p->timings.hsync + p->timings.hback + 2;  // 2 is the HDMI Latency

  h_begin = h_begin % venc_total_pixels;  // wrap around if needed
  h_end = h_begin + venc_active_pixels;
  h_end = h_end % venc_total_pixels;  // wrap around if needed
  WRITE32_REG(VPU, VPU_ENCP_DE_H_BEGIN, h_begin);
  WRITE32_REG(VPU, VPU_ENCP_DE_H_END, h_end);

  // VActive Start/End
  v_begin = p->timings.vsync + p->timings.vback;
  v_end = v_begin + active_lines;
  WRITE32_REG(VPU, VPU_ENCP_DE_V_BEGIN_EVEN, v_begin);
  WRITE32_REG(VPU, VPU_ENCP_DE_V_END_EVEN, v_end);

  if (p->timings.interlace_mode) {
    // TODO: Add support for interlace mode
    // We should not even get here
    zxlogf(ERROR, "Interlace mode not supported");
  }

  // HSync Timings
  hs_begin = h_end + venc_fp;
  if (hs_begin >= venc_total_pixels) {
    hs_begin -= venc_total_pixels;
    vsync_adjust = 1;
  }

  hs_end = hs_begin + venc_hsync;
  hs_end = hs_end % venc_total_pixels;
  WRITE32_REG(VPU, VPU_ENCP_DVI_HSO_BEGIN, hs_begin);
  WRITE32_REG(VPU, VPU_ENCP_DVI_HSO_END, hs_end);

  // VSync Timings
  if (v_begin >= (p->timings.vback + p->timings.vsync + (1 - vsync_adjust))) {
    vs_begin = v_begin - p->timings.vback - p->timings.vsync - (1 - vsync_adjust);
  } else {
    vs_begin =
        p->timings.vtotal + v_begin - p->timings.vback - p->timings.vsync - (1 - vsync_adjust);
  }
  vs_end = vs_begin + p->timings.vsync;
  vs_end = vs_end % total_lines;

  WRITE32_REG(VPU, VPU_ENCP_DVI_VSO_BLINE_EVN, vs_begin);
  WRITE32_REG(VPU, VPU_ENCP_DVI_VSO_ELINE_EVN, vs_end);
  WRITE32_REG(VPU, VPU_ENCP_DVI_VSO_BEGIN_EVN, hs_begin);
  WRITE32_REG(VPU, VPU_ENCP_DVI_VSO_END_EVN, hs_begin);

  WRITE32_REG(VPU, VPU_HDMI_SETTING, 0);
  // hsync, vsync active high. output CbYCr (GRB)
  // TODO: output desired format is hardcoded here to CbYCr (GRB)
  WRITE32_REG(VPU, VPU_HDMI_SETTING, (p->timings.hpol << 2) | (p->timings.vpol << 3) | (4 << 5));

  if (p->timings.venc_pixel_repeat) {
    SET_BIT32(VPU, VPU_HDMI_SETTING, 1, 8, 1);
  }

  // Select ENCP data to HDMI
  VpuHdmiSettingReg::Get().ReadFrom(&(*vpu_mmio_)).set_src_sel(2).WriteTo(&(*vpu_mmio_));

  zxlogf(INFO, "done");
}

void HdmiHost::ConfigPhy() {
  auto* p = &p_;

  HhiHdmiPhyCntl0Reg::Get().FromValue(0).WriteTo(&(*hhi_mmio_));
  HhiHdmiPhyCntl1Reg::Get()
      .ReadFrom(&(*hhi_mmio_))
      .set_hdmi_tx_phy_soft_reset(0)
      .set_hdmi_tx_phy_clk_en(0)
      .set_hdmi_fifo_enable(0)
      .set_hdmi_fifo_wr_enable(0)
      .set_msb_lsb_swap(0)
      .set_bit_invert(0)
      .set_ch0_swap(0)
      .set_ch1_swap(1)
      .set_ch2_swap(2)
      .set_ch3_swap(3)
      .set_new_prbs_en(0)
      .set_new_prbs_sel(0)
      .set_new_prbs_prbsmode(0)
      .set_new_prbs_mode(0)
      .WriteTo(&(*hhi_mmio_));

  HhiHdmiPhyCntl1Reg::Get()
      .ReadFrom(&(*hhi_mmio_))
      .set_hdmi_tx_phy_soft_reset(1)
      .set_hdmi_tx_phy_clk_en(1)
      .set_hdmi_fifo_enable(1)
      .set_hdmi_fifo_wr_enable(1)
      .WriteTo(&(*hhi_mmio_));
  usleep(2);
  HhiHdmiPhyCntl1Reg::Get()
      .ReadFrom(&(*hhi_mmio_))
      .set_hdmi_tx_phy_soft_reset(0)
      .set_hdmi_tx_phy_clk_en(1)
      .set_hdmi_fifo_enable(1)
      .set_hdmi_fifo_wr_enable(1)
      .WriteTo(&(*hhi_mmio_));
  usleep(2);
  HhiHdmiPhyCntl1Reg::Get()
      .ReadFrom(&(*hhi_mmio_))
      .set_hdmi_tx_phy_soft_reset(1)
      .set_hdmi_tx_phy_clk_en(1)
      .set_hdmi_fifo_enable(1)
      .set_hdmi_fifo_wr_enable(1)
      .WriteTo(&(*hhi_mmio_));
  usleep(2);
  HhiHdmiPhyCntl1Reg::Get()
      .ReadFrom(&(*hhi_mmio_))
      .set_hdmi_tx_phy_soft_reset(0)
      .set_hdmi_tx_phy_clk_en(1)
      .set_hdmi_fifo_enable(1)
      .set_hdmi_fifo_wr_enable(1)
      .WriteTo(&(*hhi_mmio_));
  usleep(2);

  switch (p->phy_mode) {
    case 1: /* 5.94Gbps, 3.7125Gbsp */
      HhiHdmiPhyCntl0Reg::Get().FromValue(0).set_hdmi_ctl1(0x37eb).set_hdmi_ctl2(0x65c4).WriteTo(
          &(*hhi_mmio_));
      HhiHdmiPhyCntl3Reg::Get().FromValue(0x2ab0ff3b).WriteTo(&(*hhi_mmio_));
      HhiHdmiPhyCntl5Reg::Get().FromValue(0x0000080b).WriteTo(&(*hhi_mmio_));
      break;
    case 2: /* 2.97Gbps */
      HhiHdmiPhyCntl0Reg::Get().FromValue(0).set_hdmi_ctl1(0x33eb).set_hdmi_ctl2(0x6262).WriteTo(
          &(*hhi_mmio_));
      HhiHdmiPhyCntl3Reg::Get().FromValue(0x2ab0ff3b).WriteTo(&(*hhi_mmio_));
      HhiHdmiPhyCntl5Reg::Get().FromValue(0x00000003).WriteTo(&(*hhi_mmio_));
      break;
    default: /* 1.485Gbps, and below */
      HhiHdmiPhyCntl0Reg::Get().FromValue(0).set_hdmi_ctl1(0x33eb).set_hdmi_ctl2(0x4242).WriteTo(
          &(*hhi_mmio_));
      HhiHdmiPhyCntl3Reg::Get().FromValue(0x2ab0ff3b).WriteTo(&(*hhi_mmio_));
      HhiHdmiPhyCntl5Reg::Get().FromValue(0x00000003).WriteTo(&(*hhi_mmio_));
      break;
  }
  usleep(20);
  zxlogf(INFO, "done!");
}

}  // namespace amlogic_display
