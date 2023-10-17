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
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"

namespace amlogic_display {

namespace {

fuchsia_hardware_hdmi::wire::DisplayMode ToHdmiFidlDisplayMode(
    fidl::AnyArena& allocator, const display::DisplayTiming& display_timing_params,
    const fuchsia_hardware_hdmi::wire::ColorParam& color) {
  fuchsia_hardware_hdmi::wire::StandardDisplayMode mode =
      display::ToHdmiFidlStandardDisplayMode(display_timing_params);
  return fuchsia_hardware_hdmi::wire::DisplayMode::Builder(allocator)
      .mode(mode)
      .color(color)
      .Build();
}

// `mode` must be a mode supported by `HdmiHost`.
cea_timing CalculateDisplayTimings(const display::DisplayTiming& mode) {
  cea_timing timings;

  timings.interlace_mode = mode.fields_per_frame == display::FieldsPerFrame::kInterlaced;
  timings.pfreq_khz = mode.pixel_clock_frequency_khz;
  // TODO: pixel repetition is 0 for most progressive. We don't support interlaced
  timings.pixel_repeat = false;
  timings.hactive = mode.horizontal_active_px;
  timings.hblank = mode.horizontal_blank_px();
  timings.hfront = mode.horizontal_front_porch_px;
  timings.hsync = mode.horizontal_sync_width_px;
  timings.htotal = mode.horizontal_total_px();
  timings.hback = mode.horizontal_back_porch_px;
  timings.hpol = mode.hsync_polarity == display::SyncPolarity::kPositive;

  timings.vactive = mode.vertical_active_lines;
  timings.vblank0 = mode.vertical_blank_lines();
  timings.vfront = mode.vertical_front_porch_lines;
  timings.vsync = mode.vertical_sync_width_lines;
  timings.vtotal = mode.vertical_total_lines();
  timings.vback = mode.vertical_back_porch_lines;
  timings.vpol = mode.vsync_polarity == display::SyncPolarity::kPositive;

  // FIXE: VENC Repeat is undocumented. It seems to be only needed for the following
  // resolutions: 1280x720p60, 1280x720p50, 720x480p60, 720x480i60, 720x576p50, 720x576i50
  // For now, we will simply not support this feature.
  timings.venc_pixel_repeat = false;

  return timings;
}

// `mode` must be a mode supported by `HdmiHost`.
pll_param CalculateClockParameters(const display::DisplayTiming& mode) {
  pll_param params;

  // TODO: We probably need a more sophisticated method for calculating
  // clocks. This will do for now.
  params.viu_channel = 1;
  params.viu_type = VIU_ENCP;
  params.vid_pll_div = VID_PLL_DIV_5;
  params.vid_clk_div = 2;
  params.hdmi_tx_pixel_div = 1;
  params.encp_div = 1;
  params.od1 = 1;
  params.od2 = 1;
  params.od3 = 1;

  params.hpll_clk_out = (mode.pixel_clock_frequency_khz * 10);
  while (params.hpll_clk_out < 2900000) {
    if (params.od1 < 4) {
      params.od1 *= 2;
      params.hpll_clk_out *= 2;
    } else if (params.od2 < 4) {
      params.od2 *= 2;
      params.hpll_clk_out *= 2;
    } else if (params.od3 < 4) {
      params.od3 *= 2;
      params.hpll_clk_out *= 2;
    } else {
      ZX_DEBUG_ASSERT_MSG(false,
                          "Failed to set HDMI PLL to a valid VCO frequency range for pixel clock "
                          "%d kHz. This should never happen since IsDisplayModeSupported() "
                          "returned true.",
                          mode.pixel_clock_frequency_khz);
    }
  }
  ZX_DEBUG_ASSERT_MSG(params.hpll_clk_out <= 6000000,
                      "Calculated HDMI PLL VCO frequency (%" PRIu32
                      " kHz) exceeds the VCO frequency limit 6GHz. This should never happen since "
                      "IsDisplayModeSupported() returned true.",
                      params.hpll_clk_out);
  return params;
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

zx_status_t HdmiHost::ModeSet(const display::DisplayTiming& mode) {
  if (!IsDisplayModeSupported(mode)) {
    zxlogf(ERROR,
           "Display mode (%" PRIu32 " x %" PRIu32 " @ pixel rate %" PRIu32
           " kHz) is not supported.",
           mode.horizontal_active_px, mode.vertical_active_lines, mode.pixel_clock_frequency_khz);
    return ZX_ERR_NOT_SUPPORTED;
  }

  cea_timing timings = CalculateDisplayTimings(mode);
  pll_param clock_params = CalculateClockParameters(mode);
  ConfigurePll(clock_params);

  WRITE32_REG(VPU, VPU_ENCP_VIDEO_EN, 0);
  WRITE32_REG(VPU, VPU_ENCI_VIDEO_EN, 0);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_MODE, 0x4040);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_MODE_ADV, 0x18);

  // Connect both VIUs (Video Input Units) to the Progressive Encoder (ENCP),
  // assuming the display is progressive.
  VideoInputUnitEncoderMuxControl::Get()
      .ReadFrom(&*vpu_mmio_)
      .set_vsync_shared_by_viu_blocks(false)
      .set_viu1_encoder_selection(VideoInputUnitEncoderMuxControl::Encoder::kProgressive)
      .set_viu2_encoder_selection(VideoInputUnitEncoderMuxControl::Encoder::kProgressive)
      .WriteTo(&*vpu_mmio_);

  WRITE32_REG(VPU, VPU_ENCP_VIDEO_VSO_BEGIN, 16);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_VSO_END, 32);

  WRITE32_REG(VPU, VPU_ENCI_VIDEO_EN, 0);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_EN, 1);

  WRITE32_REG(VPU, VPU_ENCP_VIDEO_MAX_PXCNT,
              (timings.venc_pixel_repeat) ? ((timings.htotal << 1) - 1) : (timings.htotal - 1));
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_MAX_LNCNT, timings.vtotal - 1);

  if (timings.venc_pixel_repeat) {
    SET_BIT32(VPU, VPU_ENCP_VIDEO_MODE_ADV, 1, 0, 1);
  }

  // Configure Encoder with detailed timing info (based on resolution)
  ConfigEncoder(timings);

  // Configure VDAC
  WRITE32_REG(HHI, HHI_VDAC_CNTL0_G12A, 0);
  WRITE32_REG(HHI, HHI_VDAC_CNTL1_G12A, 8);  // set Cdac_pwd [whatever that is]

  fidl::Arena<2048> allocator;
  fuchsia_hardware_hdmi::wire::DisplayMode hdmi_fidl_mode =
      ToHdmiFidlDisplayMode(allocator, mode, color_);
  auto res = hdmi_->ModeSet(1, hdmi_fidl_mode);  // only supports 1 display for now
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
  auto ops = std::make_unique<fuchsia_hardware_hdmi::wire::EdidOp[]>(op_count);
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
  auto all_ops =
      fidl::VectorView<fuchsia_hardware_hdmi::wire::EdidOp>::FromExternal(ops.get(), op_count);
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

bool HdmiHost::IsDisplayModeSupported(const display::DisplayTiming& mode) const {
  // TODO(fxbug.dev/124984): High-resolution display modes (4K or more) are not
  // supported.
  const int kMaximumAllowedWidthPixels = 2560;
  const int kMaximumAllowedHeightPixels = 1600;

  if (mode.horizontal_active_px > kMaximumAllowedWidthPixels ||
      mode.vertical_active_lines > kMaximumAllowedHeightPixels) {
    return false;
  }

  // TODO(fxbug.dev/133248): Interlaced modes are not supported.
  if (mode.fields_per_frame == display::FieldsPerFrame::kInterlaced) {
    return false;
  }

  // TODO(fxbug.dev/133248): Interlaced modes with alternating vblanks are not
  // supported.
  if (mode.vblank_alternates) {
    return false;
  }

  // TODO(fxbug.dev/134708): Modes with pixel repetition are not supported.
  if (mode.pixel_repetition != 0) {
    return false;
  }

  if (!IsPixelClockSupported(mode.pixel_clock_frequency_khz)) {
    return false;
  }

  return true;
}

void HdmiHost::ConfigEncoder(const cea_timing& timings) {
  int active_lines = (timings.vactive / (1 + timings.interlace_mode));
  int total_lines = (active_lines + timings.vblank0) +
                    ((active_lines + timings.vblank1) * timings.interlace_mode);

  // If the pixels are repeated at the HDMI transmitter, the encoder should
  // be configured to half the frequency for horizontal active pixels / blanks.
  const int kHdmiTransmitterPixelRepeatDivisionFactor = timings.pixel_repeat ? 2 : 1;

  // If the pixels are repeated at the encoder, the encoder should be configured
  // to double the frequency for horizontal active pixels / blanks.
  const int kEncoderPixelRepeatMultiplicationFactor = timings.venc_pixel_repeat ? 2 : 1;

  int venc_total_pixels = timings.htotal / kHdmiTransmitterPixelRepeatDivisionFactor *
                          kEncoderPixelRepeatMultiplicationFactor;
  int venc_active_pixels = timings.hactive / kHdmiTransmitterPixelRepeatDivisionFactor *
                           kEncoderPixelRepeatMultiplicationFactor;
  int venc_fp = timings.hfront / kHdmiTransmitterPixelRepeatDivisionFactor *
                kEncoderPixelRepeatMultiplicationFactor;
  int venc_hsync = timings.hsync / kHdmiTransmitterPixelRepeatDivisionFactor *
                   kEncoderPixelRepeatMultiplicationFactor;

  SET_BIT32(VPU, VPU_ENCP_VIDEO_MODE, 1, 14, 1);  // DE Signal polarity
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_HAVON_BEGIN, timings.hsync + timings.hback);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_HAVON_END, timings.hsync + timings.hback + timings.hactive - 1);

  WRITE32_REG(VPU, VPU_ENCP_VIDEO_VAVON_BLINE, timings.vsync + timings.vback);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_VAVON_ELINE, timings.vsync + timings.vback + timings.vactive - 1);

  WRITE32_REG(VPU, VPU_ENCP_VIDEO_HSO_BEGIN, 0);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_HSO_END, timings.hsync);

  WRITE32_REG(VPU, VPU_ENCP_VIDEO_VSO_BLINE, 0);
  WRITE32_REG(VPU, VPU_ENCP_VIDEO_VSO_ELINE, timings.vsync);

  // Below calculations assume no pixel repeat and progressive mode.
  // HActive Start/End
  int h_begin = timings.hsync + timings.hback + 2;  // 2 is the HDMI Latency
  h_begin = h_begin % venc_total_pixels;            // wrap around if needed

  int h_end = h_begin + venc_active_pixels;
  h_end = h_end % venc_total_pixels;  // wrap around if needed

  WRITE32_REG(VPU, VPU_ENCP_DE_H_BEGIN, h_begin);
  WRITE32_REG(VPU, VPU_ENCP_DE_H_END, h_end);

  // VActive Start/End
  int v_begin = timings.vsync + timings.vback;
  int v_end = v_begin + active_lines;

  WRITE32_REG(VPU, VPU_ENCP_DE_V_BEGIN_EVEN, v_begin);
  WRITE32_REG(VPU, VPU_ENCP_DE_V_END_EVEN, v_end);

  if (timings.interlace_mode) {
    // TODO: Add support for interlace mode
    // We should not even get here
    zxlogf(ERROR, "Interlace mode not supported");
  }

  // HSync Timings
  int hs_begin = h_end + venc_fp;
  int vsync_adjust = 0;
  if (hs_begin >= venc_total_pixels) {
    hs_begin -= venc_total_pixels;
    vsync_adjust = 1;
  }

  int hs_end = hs_begin + venc_hsync;
  hs_end = hs_end % venc_total_pixels;
  WRITE32_REG(VPU, VPU_ENCP_DVI_HSO_BEGIN, hs_begin);
  WRITE32_REG(VPU, VPU_ENCP_DVI_HSO_END, hs_end);

  // VSync Timings
  int vs_begin;
  if (v_begin >= (timings.vback + timings.vsync + (1 - vsync_adjust))) {
    vs_begin = v_begin - timings.vback - timings.vsync - (1 - vsync_adjust);
  } else {
    vs_begin = timings.vtotal + v_begin - timings.vback - timings.vsync - (1 - vsync_adjust);
  }
  int vs_end = vs_begin + timings.vsync;
  vs_end = vs_end % total_lines;

  WRITE32_REG(VPU, VPU_ENCP_DVI_VSO_BLINE_EVN, vs_begin);
  WRITE32_REG(VPU, VPU_ENCP_DVI_VSO_ELINE_EVN, vs_end);
  WRITE32_REG(VPU, VPU_ENCP_DVI_VSO_BEGIN_EVN, hs_begin);
  WRITE32_REG(VPU, VPU_ENCP_DVI_VSO_END_EVN, hs_begin);

  WRITE32_REG(VPU, VPU_HDMI_SETTING, 0);
  // hsync, vsync active high. output CbYCr (GRB)
  // TODO: output desired format is hardcoded here to CbYCr (GRB)
  WRITE32_REG(VPU, VPU_HDMI_SETTING, (timings.hpol << 2) | (timings.vpol << 3) | (4 << 5));

  if (timings.venc_pixel_repeat) {
    SET_BIT32(VPU, VPU_HDMI_SETTING, 1, 8, 1);
  }

  // Select ENCP data to HDMI
  VpuHdmiSettingReg::Get().ReadFrom(&(*vpu_mmio_)).set_src_sel(2).WriteTo(&(*vpu_mmio_));

  zxlogf(INFO, "done");
}

void HdmiHost::ConfigPhy() {
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

  // The following configuration for HDMI PHY control register 0, 3 and 5 only
  // works for display modes where the display resolution is lower than
  // 3840 x 2160. The configuration currently works for all display modes
  // supported by this driver.
  //
  // TODO(fxbug.dev/124984): Set the PHY control registers properly if the
  // display uses a 4k resolution (3840 x 2160 or higher).
  HhiHdmiPhyCntl0Reg::Get().FromValue(0).set_hdmi_ctl1(0x33eb).set_hdmi_ctl2(0x4242).WriteTo(
      &(*hhi_mmio_));
  HhiHdmiPhyCntl3Reg::Get().FromValue(0x2ab0ff3b).WriteTo(&(*hhi_mmio_));
  HhiHdmiPhyCntl5Reg::Get().FromValue(0x00000003).WriteTo(&(*hhi_mmio_));

  usleep(20);
  zxlogf(INFO, "done!");
}

}  // namespace amlogic_display
