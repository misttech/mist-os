// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/clock.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>

#include <cstdint>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/amlogic-display/board-resources.h"
#include "src/graphics/display/drivers/amlogic-display/clock-regs.h"
#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/dsi.h"
#include "src/graphics/display/drivers/amlogic-display/fixed-point-util.h"
#include "src/graphics/display/drivers/amlogic-display/hhi-regs.h"
#include "src/graphics/display/drivers/amlogic-display/vpu-regs.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"

namespace amlogic_display {

Clock::Clock(fdf::MmioBuffer vpu_mmio, fdf::MmioBuffer hhi_mmio, bool clock_enabled)
    : vpu_mmio_(std::move(vpu_mmio)),
      hhi_mmio_(std::move(hhi_mmio)),
      clock_enabled_(clock_enabled) {}

// static
LcdTiming Clock::CalculateLcdTiming(const display::DisplayTiming& d) {
  LcdTiming out;
  // Calculate and store DataEnable horizontal and vertical start/stop times
  const uint32_t de_hstart = d.horizontal_total_px() - d.horizontal_active_px - 1;
  const uint32_t de_vstart = d.vertical_total_lines() - d.vertical_active_lines;
  out.vid_pixel_on = de_hstart;
  out.vid_line_on = de_vstart;

  // Calculate and Store HSync horizontal and vertical start/stop times
  const uint32_t hstart = (de_hstart + d.horizontal_total_px() - d.horizontal_back_porch_px -
                           d.horizontal_sync_width_px) %
                          d.horizontal_total_px();
  const uint32_t hend =
      (de_hstart + d.horizontal_total_px() - d.horizontal_back_porch_px) % d.horizontal_total_px();
  out.hs_hs_addr = hstart;
  out.hs_he_addr = hend;

  // Calculate and Store VSync horizontal and vertical start/stop times
  out.vs_hs_addr = (hstart + d.horizontal_total_px()) % d.horizontal_total_px();
  out.vs_he_addr = out.vs_hs_addr;
  const uint32_t vstart = (de_vstart + d.vertical_total_lines() - d.vertical_back_porch_lines -
                           d.vertical_sync_width_lines) %
                          d.vertical_total_lines();
  const uint32_t vend = (de_vstart + d.vertical_total_lines() - d.vertical_back_porch_lines) %
                        d.vertical_total_lines();
  out.vs_vs_addr = vstart;
  out.vs_ve_addr = vend;
  return out;
}

zx::result<> Clock::WaitForHdmiPllToLock() {
  uint32_t pll_lock;

  constexpr int kMaxPllLockAttempt = 3;
  for (int lock_attempts = 0; lock_attempts < kMaxPllLockAttempt; lock_attempts++) {
    FDF_LOG(TRACE, "Waiting for PLL Lock: (%d/3).", lock_attempts + 1);

    // The configurations used in retries are from Amlogic-provided code which
    // is undocumented.
    if (lock_attempts == 1) {
      hhi_mmio_.Write32(
          SetFieldValue32(hhi_mmio_.Read32(HHI_HDMI_PLL_CNTL3), /*field_begin_bit=*/31,
                          /*field_size_bits=*/1, /*field_value=*/1),
          HHI_HDMI_PLL_CNTL3);
    } else if (lock_attempts == 2) {
      hhi_mmio_.Write32(0x55540000, HHI_HDMI_PLL_CNTL6);  // more magic
    }

    int retries = 1000;
    while ((pll_lock = GetFieldValue32(hhi_mmio_.Read32(HHI_HDMI_PLL_CNTL0),
                                       /*field_begin_bit=*/LCD_PLL_LOCK_HPLL_G12A,
                                       /*field_size_bits=*/1)) != 1 &&
           retries--) {
      zx_nanosleep(zx_deadline_after(ZX_USEC(50)));
    }
    if (pll_lock) {
      return zx::ok();
    }
  }

  FDF_LOG(ERROR, "Failed to lock HDMI PLL after %d attempts.", kMaxPllLockAttempt);
  return zx::error(ZX_ERR_UNAVAILABLE);
}

// static
zx::result<HdmiPllConfigForMipiDsi> Clock::GenerateHPLL(
    int64_t pixel_clock_frequency_hz, int64_t maximum_per_data_lane_bit_per_second) {
  HdmiPllConfigForMipiDsi pll_cfg;
  // Requested Pixel clock
  if (pixel_clock_frequency_hz > kMaxPixelClockFrequencyHz) {
    FDF_LOG(ERROR, "Pixel clock out of range (%" PRId64 " Hz)", pixel_clock_frequency_hz);
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  constexpr uint32_t kMinClockFactor = 1u;
  constexpr uint32_t kMaxClockFactor = 255u;

  // If clock factor is not specified in display panel configuration, the driver
  // will find the first valid clock factor in between kMinClockFactor and
  // kMaxClockFactor (both inclusive).
  uint32_t clock_factor_min = kMinClockFactor;
  uint32_t clock_factor_max = kMaxClockFactor;

  for (uint32_t clock_factor = clock_factor_min; clock_factor <= clock_factor_max; clock_factor++) {
    pll_cfg.clock_factor = clock_factor;

    // Desired PLL Frequency based on pixel clock needed
    const int64_t requested_pll_frequency_hz = pixel_clock_frequency_hz * pll_cfg.clock_factor;

    // Make sure all clocks are within range
    // If these values are not within range, we will not have a valid display
    const int64_t dphy_data_lane_bit_rate_max_hz = maximum_per_data_lane_bit_per_second;
    const int64_t dphy_data_lane_bit_rate_min_hz =
        dphy_data_lane_bit_rate_max_hz - pixel_clock_frequency_hz;
    if ((requested_pll_frequency_hz < dphy_data_lane_bit_rate_min_hz) ||
        (requested_pll_frequency_hz > dphy_data_lane_bit_rate_max_hz)) {
      FDF_LOG(TRACE, "Calculated clocks out of range for xd = %u, skipped", clock_factor);
      continue;
    }

    // Now that we have valid frequency ranges, let's calculated all the PLL-related
    // multipliers/dividers
    // [fin] * [m/n] = [voltage_controlled_oscillator_output_frequency_hz]
    // [voltage_controlled_oscillator_output_frequency_khz] / [output_divider1]
    // / [output_divider2] / [output_divider3]
    // = requested_pll_frequency
    //
    // The PLL output is used for the MIPI D-PHY clock lane; its frequency
    // must be equal to the MIPI D-PHY data lane bit rate.
    int32_t output_divider3 = (1 << (MAX_OD_SEL - 1));
    while (output_divider3 != 0) {
      const int64_t output_divider3_input_frequency_hz =
          requested_pll_frequency_hz * output_divider3;
      int output_divider2 = output_divider3;
      while (output_divider2 != 0) {
        const int64_t output_divider2_input_frequency_hz =
            output_divider3_input_frequency_hz * output_divider2;
        int32_t output_divider1 = output_divider2;
        while (output_divider1 != 0) {
          const int64_t output_divider1_input_frequency_hz =
              output_divider2_input_frequency_hz * output_divider1;
          const int64_t voltage_controlled_oscillator_output_frequency_hz =
              output_divider1_input_frequency_hz;

          if ((voltage_controlled_oscillator_output_frequency_hz >=
               kMinVoltageControlledOscillatorFrequencyHz) &&
              (voltage_controlled_oscillator_output_frequency_hz <=
               kMaxVoltageControlledOscillatorFrequencyHz)) {
            // within range!
            pll_cfg.output_divider1_selection = output_divider1 >> 1;
            pll_cfg.output_divider2_selection = output_divider2 >> 1;
            pll_cfg.output_divider3_selection = output_divider3 >> 1;
            pll_cfg.pll_frequency_hz = requested_pll_frequency_hz;
            FDF_LOG(TRACE, "od1=%d, od2=%d, od3=%d", (output_divider1 >> 1), (output_divider2 >> 1),
                    (output_divider3 >> 1));
            FDF_LOG(TRACE, "pll_fvco=%" PRId64, voltage_controlled_oscillator_output_frequency_hz);
            pll_cfg.pll_voltage_controlled_oscillator_output_frequency_hz =
                voltage_controlled_oscillator_output_frequency_hz;

            // For simplicity, assume pll_divider = 1.
            pll_cfg.pll_divider = 1;

            // Calculate pll_multiplier such that
            // kExternalOscillatorFrequencyHz x pll_multiplier =
            // voltage_controlled_oscillator_output_frequency_hz
            pll_cfg.pll_multiplier_integer = static_cast<int32_t>(
                voltage_controlled_oscillator_output_frequency_hz / kExternalOscillatorFrequencyHz);
            pll_cfg.pll_multiplier_fraction = (voltage_controlled_oscillator_output_frequency_hz %
                                               kExternalOscillatorFrequencyHz) *
                                              PLL_FRAC_RANGE / kExternalOscillatorFrequencyHz;

            FDF_LOG(TRACE, "m=%d, n=%d, frac=0x%x", pll_cfg.pll_multiplier_integer,
                    pll_cfg.pll_divider, pll_cfg.pll_multiplier_fraction);
            pll_cfg.dphy_data_lane_bits_per_second = pll_cfg.pll_frequency_hz;  // Hz

            return zx::ok(std::move(pll_cfg));
          }
          output_divider1 >>= 1;
        }
        output_divider2 >>= 1;
      }
      output_divider3 >>= 1;
    }
  }

  FDF_LOG(ERROR, "Could not generate correct PLL values for: ");
  FDF_LOG(ERROR, "  pixel_clock_frequency_hz = %" PRId64, pixel_clock_frequency_hz);
  FDF_LOG(ERROR, "  max_per_data_lane_bit_rate_hz = %" PRId64,
          maximum_per_data_lane_bit_per_second);
  return zx::error(ZX_ERR_INTERNAL);
}

void Clock::Disable() {
  if (!clock_enabled_) {
    return;
  }
  vpu_mmio_.Write32(0, ENCL_VIDEO_EN);

  VideoClockOutputControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_encoder_lvds_enabled(false)
      .WriteTo(&hhi_mmio_);
  VideoClock2Control::Get()
      .ReadFrom(&hhi_mmio_)
      .set_div1_enabled(false)
      .set_div2_enabled(false)
      .set_div4_enabled(false)
      .set_div6_enabled(false)
      .set_div12_enabled(false)
      .WriteTo(&hhi_mmio_);
  VideoClock2Control::Get().ReadFrom(&hhi_mmio_).set_clock_enabled(false).WriteTo(&hhi_mmio_);

  // disable pll
  hhi_mmio_.Write32(SetFieldValue32(hhi_mmio_.Read32(HHI_HDMI_PLL_CNTL0),
                                    /*field_begin_bit=*/LCD_PLL_EN_HPLL_G12A,
                                    /*field_size_bits=*/1, /*field_value=*/0),
                    HHI_HDMI_PLL_CNTL0);
  clock_enabled_ = false;
}

zx::result<> Clock::Enable(const PanelConfig& panel_config) {
  if (clock_enabled_) {
    return zx::ok();
  }

  // Populate internal LCD timing structure based on predefined tables
  lcd_timing_ = CalculateLcdTiming(panel_config.display_timing);
  zx::result<HdmiPllConfigForMipiDsi> pll_result =
      GenerateHPLL(panel_config.display_timing.pixel_clock_frequency_hz,
                   panel_config.maximum_per_data_lane_bit_per_second());
  if (pll_result.is_error()) {
    FDF_LOG(ERROR, "Failed to generate HDMI PLL and Video clock tree configuration: %s",
            pll_result.status_string());
    return pll_result.take_error();
  }
  pll_cfg_ = std::move(pll_result).value();

  uint32_t regVal;
  bool useFrac = pll_cfg_.pll_multiplier_fraction != 0;

  regVal = ((1 << LCD_PLL_EN_HPLL_G12A) | (1 << LCD_PLL_OUT_GATE_CTRL_G12A) |  // clk out gate
            (pll_cfg_.pll_divider << LCD_PLL_N_HPLL_G12A) |
            (pll_cfg_.pll_multiplier_integer << LCD_PLL_M_HPLL_G12A) |
            (pll_cfg_.output_divider1_selection << LCD_PLL_OD1_HPLL_G12A) |
            (pll_cfg_.output_divider2_selection << LCD_PLL_OD2_HPLL_G12A) |
            (pll_cfg_.output_divider3_selection << LCD_PLL_OD3_HPLL_G12A) |
            (useFrac ? (1 << 27) : (0 << 27)));
  hhi_mmio_.Write32(regVal, HHI_HDMI_PLL_CNTL0);

  hhi_mmio_.Write32(pll_cfg_.pll_multiplier_fraction, HHI_HDMI_PLL_CNTL1);
  hhi_mmio_.Write32(0x00, HHI_HDMI_PLL_CNTL2);
  // Magic numbers from U-Boot.
  hhi_mmio_.Write32(useFrac ? 0x6a285c00 : 0x48681c00, HHI_HDMI_PLL_CNTL3);
  hhi_mmio_.Write32(useFrac ? 0x65771290 : 0x33771290, HHI_HDMI_PLL_CNTL4);
  hhi_mmio_.Write32(0x39272000, HHI_HDMI_PLL_CNTL5);
  hhi_mmio_.Write32(useFrac ? 0x56540000 : 0x56540000, HHI_HDMI_PLL_CNTL6);

  // reset dpll
  hhi_mmio_.Write32(SetFieldValue32(hhi_mmio_.Read32(HHI_HDMI_PLL_CNTL0),
                                    /*field_begin_bit=*/LCD_PLL_RST_HPLL_G12A,
                                    /*field_size_bits=*/1, /*field_value=*/1),
                    HHI_HDMI_PLL_CNTL0);
  zx_nanosleep(zx_deadline_after(ZX_USEC(100)));
  // release from reset
  hhi_mmio_.Write32(SetFieldValue32(hhi_mmio_.Read32(HHI_HDMI_PLL_CNTL0),
                                    /*field_begin_bit=*/LCD_PLL_RST_HPLL_G12A,
                                    /*field_size_bits=*/1, /*field_value=*/0),
                    HHI_HDMI_PLL_CNTL0);

  zx_nanosleep(zx_deadline_after(ZX_USEC(50)));
  zx::result<> wait_for_pll_lock_result = WaitForHdmiPllToLock();
  if (!wait_for_pll_lock_result.is_ok()) {
    FDF_LOG(ERROR, "Failed to lock HDMI PLL: %s", wait_for_pll_lock_result.status_string());
    return wait_for_pll_lock_result.take_error();
  }

  // Disable Video Clock mux 2 since we are changing its input selection.
  VideoClock2Control::Get().ReadFrom(&hhi_mmio_).set_clock_enabled(false).WriteTo(&hhi_mmio_);
  zx_nanosleep(zx_deadline_after(ZX_USEC(5)));

  // Sets the HDMI clock tree frequency division ratio to 1.

  // Disable the HDMI clock tree output
  HdmiClockTreeControl hdmi_clock_tree_control = HdmiClockTreeControl::Get().ReadFrom(&hhi_mmio_);
  hdmi_clock_tree_control.set_clock_output_enabled(false).WriteTo(&hhi_mmio_);

  hdmi_clock_tree_control.set_preset_pattern_update_enabled(false).WriteTo(&hhi_mmio_);
  hdmi_clock_tree_control.SetFrequencyDividerRatio(ToU28_4(1.0)).WriteTo(&hhi_mmio_);

  // Enable the final output clock
  hdmi_clock_tree_control.set_clock_output_enabled(true).WriteTo(&hhi_mmio_);

  // Enable DSI measure clocks.
  VideoInputMeasureClockControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_dsi_measure_clock_selection(
          VideoInputMeasureClockControl::ClockSource::kExternalOscillator24Mhz)
      .WriteTo(&hhi_mmio_);
  VideoInputMeasureClockControl::Get()
      .ReadFrom(&hhi_mmio_)
      .SetDsiMeasureClockDivider(1)
      .WriteTo(&hhi_mmio_);
  VideoInputMeasureClockControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_dsi_measure_clock_enabled(true)
      .WriteTo(&hhi_mmio_);

  // Use Video PLL (vid_pll) as MIPI_DSY PHY clock source.
  MipiDsiPhyClockControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_clock_source(MipiDsiPhyClockControl::ClockSource::kVideoPll)
      .WriteTo(&hhi_mmio_);
  // Enable MIPI-DSY PHY clock.
  MipiDsiPhyClockControl::Get().ReadFrom(&hhi_mmio_).set_enabled(true).WriteTo(&hhi_mmio_);
  // Set divider to 1.
  // TODO(https://fxbug.dev/42082049): This should occur before enabling the clock.
  MipiDsiPhyClockControl::Get().ReadFrom(&hhi_mmio_).SetDivider(1).WriteTo(&hhi_mmio_);

  // Set the Video clock 2 divider.
  VideoClock2Divider::Get()
      .ReadFrom(&hhi_mmio_)
      .SetDivider2(pll_cfg_.clock_factor)
      .WriteTo(&hhi_mmio_);
  zx_nanosleep(zx_deadline_after(ZX_USEC(5)));

  VideoClock2Control::Get()
      .ReadFrom(&hhi_mmio_)
      .set_mux_source(VideoClockMuxSource::kVideoPll)
      .WriteTo(&hhi_mmio_);
  VideoClock2Control::Get().ReadFrom(&hhi_mmio_).set_clock_enabled(true).WriteTo(&hhi_mmio_);
  zx_nanosleep(zx_deadline_after(ZX_USEC(2)));

  // Select video clock 2 for ENCL clock.
  VideoClock2Divider::Get()
      .ReadFrom(&hhi_mmio_)
      .set_encl_clock_selection(EncoderClockSource::kVideoClock2)
      .WriteTo(&hhi_mmio_);
  // Enable video clock 2 divider.
  VideoClock2Divider::Get().ReadFrom(&hhi_mmio_).set_divider_enabled(true).WriteTo(&hhi_mmio_);
  zx_nanosleep(zx_deadline_after(ZX_USEC(5)));

  VideoClock2Control::Get().ReadFrom(&hhi_mmio_).set_div1_enabled(true).WriteTo(&hhi_mmio_);
  VideoClock2Control::Get().ReadFrom(&hhi_mmio_).set_soft_reset(true).WriteTo(&hhi_mmio_);
  zx_nanosleep(zx_deadline_after(ZX_USEC(10)));
  VideoClock2Control::Get().ReadFrom(&hhi_mmio_).set_soft_reset(false).WriteTo(&hhi_mmio_);
  zx_nanosleep(zx_deadline_after(ZX_USEC(5)));

  // Enable ENCL clock output.
  VideoClockOutputControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_encoder_lvds_enabled(true)
      .WriteTo(&hhi_mmio_);

  zx_nanosleep(zx_deadline_after(ZX_MSEC(10)));

  vpu_mmio_.Write32(0, ENCL_VIDEO_EN);

  // Connect both VIUs (Video Input Units) to the LCD Encoder.
  VideoInputUnitEncoderMuxControl::Get()
      .ReadFrom(&vpu_mmio_)
      .set_vsync_shared_by_viu_blocks(false)
      .set_viu1_encoder_selection(VideoInputUnitEncoderMuxControl::Encoder::kLcd)
      .set_viu2_encoder_selection(VideoInputUnitEncoderMuxControl::Encoder::kLcd)
      .WriteTo(&vpu_mmio_);

  // Undocumented registers below
  vpu_mmio_.Write32(0x8000, ENCL_VIDEO_MODE);      // bit[15] shadown en
  vpu_mmio_.Write32(0x0418, ENCL_VIDEO_MODE_ADV);  // Sampling rate: 1

  // bypass filter -- Undocumented registers
  const display::DisplayTiming& display_timing = panel_config.display_timing;
  vpu_mmio_.Write32(0x1000, ENCL_VIDEO_FILT_CTRL);
  vpu_mmio_.Write32(display_timing.horizontal_total_px() - 1, ENCL_VIDEO_MAX_PXCNT);
  vpu_mmio_.Write32(display_timing.vertical_total_lines() - 1, ENCL_VIDEO_MAX_LNCNT);
  vpu_mmio_.Write32(lcd_timing_.vid_pixel_on, ENCL_VIDEO_HAVON_BEGIN);
  vpu_mmio_.Write32(display_timing.horizontal_active_px - 1 + lcd_timing_.vid_pixel_on,
                    ENCL_VIDEO_HAVON_END);
  vpu_mmio_.Write32(lcd_timing_.vid_line_on, ENCL_VIDEO_VAVON_BLINE);
  vpu_mmio_.Write32(display_timing.vertical_active_lines - 1 + lcd_timing_.vid_line_on,
                    ENCL_VIDEO_VAVON_ELINE);
  vpu_mmio_.Write32(lcd_timing_.hs_hs_addr, ENCL_VIDEO_HSO_BEGIN);
  vpu_mmio_.Write32(lcd_timing_.hs_he_addr, ENCL_VIDEO_HSO_END);
  vpu_mmio_.Write32(lcd_timing_.vs_hs_addr, ENCL_VIDEO_VSO_BEGIN);
  vpu_mmio_.Write32(lcd_timing_.vs_he_addr, ENCL_VIDEO_VSO_END);
  vpu_mmio_.Write32(lcd_timing_.vs_vs_addr, ENCL_VIDEO_VSO_BLINE);
  vpu_mmio_.Write32(lcd_timing_.vs_ve_addr, ENCL_VIDEO_VSO_ELINE);
  vpu_mmio_.Write32(3, ENCL_VIDEO_RGBIN_CTRL);
  vpu_mmio_.Write32(1, ENCL_VIDEO_EN);

  vpu_mmio_.Write32(0, L_RGB_BASE_ADDR);
  vpu_mmio_.Write32(0x400, L_RGB_COEFF_ADDR);
  vpu_mmio_.Write32(0x400, L_DITH_CNTL_ADDR);

  // The driver behavior here deviates from the Amlogic-provided code.
  //
  // The Amlogic-provided code sets up the timing controller (TCON) within the
  // LCD Encoder to generate Display Enable (DE), Horizontal Sync (HSYNC) and
  // Vertical Sync (VSYNC) timing signals.
  //
  // These signals are useful for LVDS or TTL LCD interfaces, but not for the
  // MIPI-DSI interface. The MIPI-DSI Host Controller IP block always sets the
  // output timings using the values from its own control registers, and it
  // doesn't use the outputs from the timing controller.
  //
  // Therefore, this driver doesn't set the timing controller registers and
  // keeps the timing controller disabled. This was tested on Nelson (S905D3)
  // and Khadas VIM3 (A311D) boards.

  vpu_mmio_.Write32(vpu_mmio_.Read32(VPP_MISC) & ~(VPP_OUT_SATURATE), VPP_MISC);

  // Ready to be used
  clock_enabled_ = true;
  return zx::ok();
}

void Clock::SetVideoOn(bool on) { vpu_mmio_.Write32(on, ENCL_VIDEO_EN); }

// static
zx::result<std::unique_ptr<Clock>> Clock::Create(
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device,
    bool already_enabled) {
  ZX_DEBUG_ASSERT(platform_device.is_valid());

  zx::result<fdf::MmioBuffer> vpu_mmio_result = MapMmio(kMmioNameVpu, platform_device);
  if (vpu_mmio_result.is_error()) {
    return vpu_mmio_result.take_error();
  }
  fdf::MmioBuffer vpu_mmio = std::move(vpu_mmio_result).value();

  zx::result<fdf::MmioBuffer> hhi_mmio_result = MapMmio(kMmioNameHhi, platform_device);
  if (hhi_mmio_result.is_error()) {
    return hhi_mmio_result.take_error();
  }
  fdf::MmioBuffer hhi_mmio = std::move(hhi_mmio_result).value();

  fbl::AllocChecker alloc_checker;
  auto clock =
      fbl::make_unique_checked<Clock>(&alloc_checker, std::move(vpu_mmio), std::move(hhi_mmio),
                                      /*clock_enabled=*/already_enabled);
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for Clock");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(clock));
}

}  // namespace amlogic_display
