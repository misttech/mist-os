// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_CLOCK_REGS_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_CLOCK_REGS_H_

#include <zircon/assert.h>

#include <cstdint>

#include <hwreg/bitfields.h>

namespace amlogic_display {

// # Amlogic Clock Subsystem
//
// On Amlogic SoCs, the clock subsystem consists of **phased-lock loops (PLLs)**
// generating clock signals using oscillators, and **clock trees** which are
// exclusively made up of digital logic (such as muxes and frequency dividers).
//
// ## Video Clock Tree
//
// The **video clock tree** is a set of muxes and frequency dividers providing
// clocks for encoders, HDMI / DSI transmitter, display timing controllers,
// and video digital-analog converters.
//
// The video clock tree takes the following clock sources:
//
// - `vid_pll`
//   Video "PLL", which is actually an output signal of the HDMI clock tree
//   taking the HDMI PLL as its source.
//   * A311D Datasheet, Section 8.7.1.3 HDMI Clock Tree, Page 113
//   * S905D2 Datasheet, Section 6.6.2.3 HDMI Clock Tree, Page 97
//   * S905D3 Datasheet, Section 6.7.2.3 HDMI Clock Tree, Page 97
// - `gp0_pll`
//   General-purpose PLL 0.
//   * A311D Datasheet, Section 8.7.2.3 GP0 PLL, Page 117
//   * S905D2 Datasheet, Section 6.6.3.3 GP0 PLL, Page 101
//   * S905D3 Datasheet, Section 6.7.3.2 GP0 PLL, Page 100
// - `hifi_pll`
//   HiFi PLL.
//   * A311D Datasheet, Section 8.7.2.5 HIFI PLL, Page 118
//   * S905D2 Datasheet, Section 6.6.3.5 HIFI PLL, Page 102
//   * S905D3 Datasheet, Section 6.7.3.3 HIFI PLL, Page 101
// - `mp1_clk`
//   Also known as `MPLL1`, `MPLL_DDS_CLK1`. The fixed-frequency PLL (MPLL,
//   also known as FIX_PLL) is divided by a programmable frequency divider,
//   providing a clock with frequency of up to 500MHz.
//   References for all MPLL / FIX_PLL outputs:
//   * A311D Datasheet, Section 8.7.2.5 MPLL (Fixed PLL), Page 119
//   * S905D2 Datasheet, Section 6.6.3.6 MPLL (Fixed PLL), Page 103
//   * S905D3 Datasheet, Section 6.7.3.7 MPLL  Page 104
// - `fclk_div3`
//   Also known as `MPLL_CLK_OUT_DIV3`. The fixed-frequency MPLL is divided by a
//   fixed-value frequency divider, providing a 666MHz fixed-frequency clock.
// - `fclk_div4`
//   Also known as `MPLL_CLK_OUT_DIV4`. The fixed-frequency MPLL is divided by a
//   fixed-value frequency divider, providing a 500MHz fixed-frequency clock.
// - `fclk_div5`
//   Also known as `MPLL_CLK_OUT_DIV5`. The fixed-frequency MPLL is divided by a
//   fixed-value frequency divider, providing a 400MHz fixed-frequency clock.
// - `fclk_div7`
//   Also known as `MPLL_CLK_OUT_DIV7`. The fixed-frequency MPLL is divided by a
//   fixed-value frequency divider, providing a 285.7MHz fixed-frequency clock.
//
// It provides the following clock signals:
// - `cts_tcon` / `tcon_clko` (Timing controller)
// - `lcd_an_clk_ph2` (LCD Analog clock for PHY2)
// - `lcd_an_clk_ph3` (LCD Analog clock for PHY3)
// - `cts_enci_clk` (ENCI (Interlaced Encoder) clock)
// - `cts_encl_clk` (ENCL (LVDS Encoder) clock)
// - `cts_encp_clk` (ENCP (Progressive Encoder) clock)
// - `hdmi_tx_pixel` (HDMI Transmitter pixel clock)
// - `cts_vdac_clk` (Video digital-analog converter clock)
//
// The following sections of the Amlogic datasheets are good for understanding
// the PLLs and the dividers that make up the clock tree sources:
// - A311D
//   * Section 8.7.1 "Clock" > "Overview" (pages 108-109) has Table 8-9 "A311D
//     PLLs" and Figure 8-6 "Clock Connections";
//   * Section 8.7.2 "Frequency Calculation and Setting" (pages 115-121) has
//     per-PLL subsections showing diagrams.
//   * Section 8.7.1.3 "HDMI Clock Tree" (pages 112-113) has the clock tree
//     diagram (Figure 8-12).
// - S905D2
//   * Section 6.6.1 "Clock" > "Overview" (page 91) has Table 6-5 "A311D
//     PLLs".
//   * Section 6.6.2 "Clock Trees" (pages 91-92) has Figure 6-5 "Clock
//     Connections";
//   * Section 6.6.3 "Frequency Calculation and Setting" (pages 99-106) has
//     per-PLL subsections showing diagrams.
//   * Section 6.6.2.3 "HDMI Clock Tree" (pages 96-97) has the clock tree
//     diagram (Figure 6-11).
// - S905D3
//   * Section 6.7.1 "Clock" > "Overview" (page 90) has Table 6-5 "A311D
//     PLLs".
//   * Section 6.7.2 "Clock Trees" (pages 91-92) has Figure 6-5 "Clock
//     Connections";
//   * Section 6.7.3 "Frequency Calculation and Setting" (pages 99-106) has
//     per-PLL subsections showing diagrams.
//   * Section 6.7.2.3 "HDMI Clock Tree" (pages 96-97) has the clock tree
//     diagram (Figure 6-11).
//
// The detailed diagram of the video clock tree is shown in the following
// section of the Amlogic datasheets:
//
// A311D Datasheet, Figure 8-13 "Video Clock Tree", Section 8.7.1.4 EE Clock
// Tree, Page 114.
// S905D2 Datasheet, Figure 6-12 "Video Clock Tree", Section 6.6.2.4 EE Clock
// Tree, Page 98.
// S905D3 Datasheet, Figure 6-13 "Video Clock Tree", Section 6.7.2.4 EE Clock
// Tree, Page 99.

// Video clock tree has two muxes for input signals, named video clock 1 / mux 1
// (VID_CLK) and video clock 2 / mux 2 (VIID_CLK / V2).
//
// Each video clock mux is followed by a programmable divisor (/N0 for mux 1
// and /N2 for mux 2) and then multiple fixed divisors (/2, /4, /6 and /12).
// Each encoder / HDMI transmitter / video digital-to-analog converter (DAC)
// clock signal has its own mux, to select a clock from those provided by the
// above divisors.
//
// The output of video clock 1 is also used to generate the timing
// controller signal and LCD analog clocks.
enum class VideoClock {
  kVideoClock1 = 1,
  kVideoClock2 = 2,
};

// Selection of video clock muxes.
//
// The mux value <-> clock source mapping is shown in the following diagram
// of the Amlogic datasheets:
// A311D Datasheet, Figure 8-13 "Video Clock Tree", Section 8.7.1.4 EE Clock
// Tree, Page 114.
// S905D2 Datasheet, Figure 6-12 "Video Clock Tree", Section 6.6.2.4 EE Clock
// Tree, Page 98.
// S905D3 Datasheet, Figure 6-13 "Video Clock Tree", Section 6.7.2.4 EE Clock
// Tree, Page 99.
enum class VideoClockMuxSource : uint32_t {
  // `vid_pll`, the video "PLL" output of the HDMI clock tree.
  kVideoPll = 0,
  // `gp0_pll`
  kGeneralPurpose0Pll = 1,
  // `hifi_pll`
  kHifiPll = 2,
  // `mp1_clk`
  kMpll1 = 3,
  // `fclk_div3`
  kFixed666Mhz = 4,
  // `fclk_div4`
  kFixed500Mhz = 5,
  // `fclk_div5`
  kFixed400Mhz = 6,
  // `fclk_div7`
  kFixed285_7Mhz = 7,
};

// Selection of video clock and dividers for encoder clock muxes.
//
// The mux value <-> clock source mapping is shown in the following diagram
// of the Amlogic datasheets:
// A311D Datasheet, Figure 8-13 "Video Clock Tree", Section 8.7.1.4 EE Clock
// Tree, Page 114.
// S905D2 Datasheet, Figure 6-12 "Video Clock Tree", Section 6.6.2.4 EE Clock
// Tree, Page 98.
// S905D3 Datasheet, Figure 6-13 "Video Clock Tree", Section 6.7.2.4 EE Clock
// Tree, Page 99.
enum class EncoderClockSource : uint32_t {
  // "VideoClock1" is first divided by the programmable divider "/N0" before
  // being divided by the fixed divider. So the actual frequency is
  // (Selected Video Clock 1 input) / (N0) / (1, 2, 4, 16, or 12)
  kVideoClock1 = 0,
  kVideoClock1Div2 = 1,
  kVideoClock1Div4 = 2,
  kVideoClock1Div6 = 3,
  kVideoClock1Div12 = 4,

  // "VideoClock2" is first divided by the programmable divider "/N2" before
  // being divided by the fixed divider. So the actual frequency is
  // (Selected Video Clock 2 input) / (N2) / (1, 2, 4, 16, or 12)
  kVideoClock2 = 8,
  kVideoClock2Div2 = 9,
  kVideoClock2Div4 = 10,
  kVideoClock2Div6 = 11,
  kVideoClock2Div12 = 12,
};

// HHI_VIID_CLK_DIV
//
// A311D Datasheet, Section 8.7.6 Register Descriptions, Page 146.
// S905D2 Datasheet, Section 6.6.6 Register Descriptions, Page 126.
// S905D3 Datasheet, Section 6.7.6 Register Descriptions, Page 131.
class VideoClock2Divider : public hwreg::RegisterBase<VideoClock2Divider, uint32_t> {
 public:
  DEF_ENUM_FIELD(EncoderClockSource, 31, 28, video_dac_clock_selection);

  // Bits 27-24 and 23-20 document the DAC1 and DAC2 clock selections in A311D,
  // S905D2 and S905D3 documentations. These clocks don't exist in the clock
  // trees and are not used by this driver, so we don't define these bits.

  // Iff true, overrides the Video DAC clock source selection in
  // `video_dac_clock_selection` field to `adc_pll_clk_b2`.
  DEF_BIT(19, video_dac_clock_selects_adc_pll_clock_b2);

  DEF_RSVDZ_BIT(18);

  // Iff true, resets the divider (/N2) for video clock 2.
  DEF_BIT(17, divider_reset);

  // Iff true, enables the divider (/N2) for video clock 2.
  DEF_BIT(16, divider_enabled);

  DEF_ENUM_FIELD(EncoderClockSource, 15, 12, encl_clock_selection);

  // Bits 14-8 are defined as unused in the datasheets, which contradicts the
  // definition of bits 15-12 in the same table. Experiments on VIM3 (A311D),
  // Astro (S905D2) and Nelson (S905D3) show that only bits 11-8 are unused.
  DEF_RSVDZ_FIELD(11, 8);

  // Also known as "/N2" in the Video Clock Tree diagram.
  //
  // It's preferred to use {Get,Set}Divider2() to get / set the divider value.
  DEF_FIELD(7, 0, divider2_minus_one);
  static constexpr int kMinDivider2 = 1;
  static constexpr int kMaxDivider2 = [] {
    constexpr int kFieldLow = 0;
    constexpr int kFieldHigh = 7;
    return 1 << (kFieldHigh - kFieldLow + 1);
  }();

  VideoClock2Divider& SetDivider2(int divider2) {
    ZX_DEBUG_ASSERT(divider2 >= kMinDivider2);
    ZX_DEBUG_ASSERT(divider2 <= kMaxDivider2);
    return set_divider2_minus_one(divider2 - 1);
  }

  int GetDivider2() const { return divider2_minus_one() + 1; }

  static auto Get() {
    constexpr uint32_t kRegAddr = 0x4a * sizeof(uint32_t);
    return hwreg::RegisterAddr<VideoClock2Divider>(kRegAddr);
  }
};

// HHI_VIID_CLK_CNTL
//
// A311D Datasheet, Section 8.7.6 Register Descriptions, Page 146.
// S905D2 Datasheet, Section 6.6.6 Register Descriptions, Page 126-127.
// S905D3 Datasheet, Section 6.7.6 Register Descriptions, Page 132.
class VideoClock2Control : public hwreg::RegisterBase<VideoClock2Control, uint32_t> {
 public:
  DEF_RSVDZ_FIELD(31, 20);

  // Gate-controls the input (also gate-controlled by the
  // `clock2_divider_enabled` bit of the `VideoClock2Divider` register) and
  // output signals for the video clock 2 divider.
  DEF_BIT(19, clock_enabled);

  DEF_ENUM_FIELD(VideoClockMuxSource, 18, 16, mux_source);

  // This is a "level triggered" signal. Drivers reset the clock dividers by
  // first setting the bit to 1, sleeping for 10 us (empirical value from VIM3
  // using Amlogic A311D chip) and then setting the bit to 0.
  DEF_BIT(15, soft_reset);

  DEF_RSVDZ_FIELD(12, 5);

  DEF_BIT(4, div12_enabled);
  DEF_BIT(3, div6_enabled);
  DEF_BIT(2, div4_enabled);
  DEF_BIT(1, div2_enabled);
  DEF_BIT(0, div1_enabled);

  static auto Get() {
    constexpr uint32_t kRegAddr = 0x4b * sizeof(uint32_t);
    return hwreg::RegisterAddr<VideoClock2Control>(kRegAddr);
  }
};

// HHI_VID_CLK_DIV
//
// A311D Datasheet, Section 8.7.6 Register Descriptions, Page 150.
// S905D2 Datasheet, Section 6.6.6 Register Descriptions, Page 136.
// S905D3 Datasheet, Section 6.7.6 Register Descriptions, Page 129.
class VideoClock1Divider : public hwreg::RegisterBase<VideoClock1Divider, uint32_t> {
 public:
  DEF_ENUM_FIELD(EncoderClockSource, 31, 28, enci_clock_selection);

  DEF_ENUM_FIELD(EncoderClockSource, 27, 24, encp_clock_selection);

  DEF_ENUM_FIELD(EncoderClockSource, 23, 20, enct_clock_selection);

  DEF_RSVDZ_FIELD(19, 18);

  // Iff true, resets the dividers (/N0 and /N1) for video clock 1.
  DEF_BIT(17, dividers_reset);

  // Iff true, enables the dividers (/N0 and /N1) for video clock 1.
  // Divider /N0 / /N1 works iff `dividers_enabled` and `divider0/1_enabled`
  // field in `VideoClock1Control` register are both true.
  DEF_BIT(16, dividers_enabled);

  // Also known as "/N1" in the Video Clock Tree diagram.
  //
  // It's preferred to use {Get,Set}Divider1() to get / set the divider value.
  DEF_FIELD(15, 8, divider1_minus_one);
  static constexpr int kMinDivider1 = 1;
  static constexpr int kMaxDivider1 = [] {
    constexpr int kFieldLow = 8;
    constexpr int kFieldHigh = 15;
    return 1 << (kFieldHigh - kFieldLow + 1);
  }();

  VideoClock1Divider& SetDivider1(int divider1) {
    ZX_DEBUG_ASSERT(divider1 >= kMinDivider1);
    ZX_DEBUG_ASSERT(divider1 <= kMaxDivider1);
    return set_divider1_minus_one(divider1 - 1);
  }

  int GetDivider1() const { return divider1_minus_one() + 1; }

  // Also known as "/N0" in the Video Clock Tree diagram.
  //
  // It's preferred to use {Get,Set}Divider0() to get / set the divider value.
  DEF_FIELD(7, 0, divider0_minus_one);
  static constexpr int kMinDivider0 = 1;
  static constexpr int kMaxDivider0 = [] {
    constexpr int kFieldLow = 0;
    constexpr int kFieldHigh = 7;
    return 1 << (kFieldHigh - kFieldLow + 1);
  }();

  VideoClock1Divider& SetDivider0(int divider0) {
    ZX_DEBUG_ASSERT(divider0 >= kMinDivider0);
    ZX_DEBUG_ASSERT(divider0 <= kMaxDivider0);
    return set_divider0_minus_one(divider0 - 1);
  }

  int GetDivider0() const { return divider0_minus_one() + 1; }

  static auto Get() {
    constexpr uint32_t kRegAddr = 0x59 * sizeof(uint32_t);
    return hwreg::RegisterAddr<VideoClock1Divider>(kRegAddr);
  }
};

// HHI_VID_CLK_CNTL
//
// A311D Datasheet, Section 8.7.6 Register Descriptions, Page 151.
// S905D2 Datasheet, Section 6.6.6 Register Descriptions, Page 137.
// S905D3 Datasheet, Section 6.7.6 Register Descriptions, Page 129-130.
class VideoClock1Control : public hwreg::RegisterBase<VideoClock1Control, uint32_t> {
 public:
  // Bits 31-21 control the clock generation module for timing controller clock
  // (cts_tcon). The subfield definition is not in the register description
  // table but is mentioned in the clock tree diagram.
  //
  // A311D Datasheet, Section 8.7.1 Clock Trees, Page 114.
  // S905D2 Datasheet, Section 6.6.2 Clock Trees, Page 98.
  // S905D3 Datasheet, Section 6.7.2 Clock Trees, Page 99.
  DEF_FIELD(31, 21, timing_controller_clock_control);

  // Gate-controls the output signal of divider /N1.
  // Divider /N1 works iff `divider1_enabled` and the `dividers_enabled`
  // field in `VideoClock1Divider` register are both true.
  DEF_BIT(20, divider1_enabled);

  // Gate-controls the output signal of divider /N0.
  // Divider /N0 works iff `divider0_enabled` and the `dividers_enabled`
  // field in `VideoClock1Divider` register are both true.
  DEF_BIT(19, divider0_enabled);

  DEF_ENUM_FIELD(VideoClockMuxSource, 18, 16, mux_source);

  // This is a "level triggered" signal. Drivers reset the clock dividers by
  // first setting the bit to 1, sleeping for 10 us (empirical value from VIM3
  // using A311D chip) and then setting the bit to 0.
  DEF_BIT(15, soft_reset);

  // Enables the mux for the clock `lcd_an_clk_ph2` and `lcd_an_clk_ph3`.
  DEF_BIT(14, lcd_analog_clock_mux_enabled);

  enum class LcdAnalogClockSelection : uint32_t {
    kVideoClock1Div6 = 0,
    kVideoClock1Div12 = 1,
  };
  // "Video Clock Tree" diagrams use the bit 11 on register 0x1a
  // (HHI_GP1_PLL_CNTL2) which doesn't match the register definitions on the
  // same datasheet. Experiments on VIM3 (Amlogic A311D) shows that this bit
  // is the correct bit to select input source for LCD analog clocks.
  DEF_ENUM_FIELD(LcdAnalogClockSelection, 13, 13, lcd_analog_clock_selection);

  DEF_RSVDZ_FIELD(12, 5);

  DEF_BIT(4, div12_enabled);
  DEF_BIT(3, div6_enabled);
  DEF_BIT(2, div4_enabled);
  DEF_BIT(1, div2_enabled);
  DEF_BIT(0, div1_enabled);

  static auto Get() {
    constexpr uint32_t kRegAddr = 0x5f * sizeof(uint32_t);
    return hwreg::RegisterAddr<VideoClock1Control>(kRegAddr);
  }
};

// HHI_VID_CLK_CNTL2
//
// A311D Datasheet, Section 8.7.6 Register Descriptions, Page 152.
// S905D2 Datasheet, Section 6.6.6 Register Descriptions, Page 137.
// S905D3 Datasheet, Section 6.7.6 Register Descriptions, Page 130.
class VideoClockOutputControl : public hwreg::RegisterBase<VideoClockOutputControl, uint32_t> {
 public:
  DEF_RSVDZ_FIELD(15, 9);

  DEF_BIT(8, analog_tv_demodulator_video_dac_clock_enabled);
  DEF_BIT(7, lcd_analog_clock_phy2_enabled);
  DEF_BIT(6, lcd_analog_clock_phy3_enabled);
  DEF_BIT(5, hdmi_tx_pixel_clock_enabled);
  DEF_BIT(4, video_dac_clock_enabled);
  DEF_BIT(3, encoder_lvds_enabled);
  DEF_BIT(2, encoder_progressive_enabled);
  DEF_BIT(1, encoder_tv_enabled);
  DEF_BIT(0, encoder_interlaced_enabled);

  static auto Get() {
    constexpr uint32_t kRegAddr = 0x65 * sizeof(uint32_t);
    return hwreg::RegisterAddr<VideoClockOutputControl>(kRegAddr);
  }
};

// HHI_HDMI_CLK_CNTL
//
// A311D Datasheet, Section 8.7.6 Register Descriptions, Page 157.
// S905D2 Datasheet, Section 6.6.6 Register Descriptions, Page 142.
// S905D3 Datasheet, Section 6.7.6 Register Descriptions, Page 133.
class HdmiClockControl : public hwreg::RegisterBase<HdmiClockControl, uint32_t> {
 public:
  DEF_RSVDZ_FIELD(31, 20);

  // Selection of video clock muxes.
  //
  // The mux value <-> clock source mapping is shown in the following diagram
  // of the Amlogic datasheets:
  // A311D Datasheet, Figure 8-13 "Video Clock Tree", Section 8.7.1.4 EE Clock
  // Tree, Page 114.
  // S905D2 Datasheet, Figure 6-12 "Video Clock Tree", Section 6.6.2.4 EE Clock
  // Tree, Page 98.
  // S905D3 Datasheet, Figure 6-13 "Video Clock Tree", Section 6.7.2.4 EE Clock
  // Tree, Page 99.
  enum class HdmiTxPixelClockSource : uint32_t {
    // "VideoClock1" is divided by the programmable divider "/N0" before
    // being divided by the fixed divider. So the actual frequency is
    // (Selected Video Clock 1 input) / (N0) / (1, 2, 4, 16, or 12)
    kVideoClock1 = 0,
    kVideoClock1Div2 = 1,
    kVideoClock1Div4 = 2,
    kVideoClock1Div6 = 3,
    kVideoClock1Div12 = 4,

    // "VideoClock2" is divided by the programmable divider "/N2" before
    // being divided by the fixed divider. So the actual frequency is
    // (Selected Video Clock 2 input) / (N2) / (1, 2, 4, 16, or 12)
    kVideoClock2 = 8,
    kVideoClock2Div2 = 9,
    kVideoClock2Div4 = 10,
    kVideoClock2Div6 = 11,
    kVideoClock2Div12 = 12,

    // `cts_tcon` clock provided by the video clock tree.
    //
    // This value is not documented in S905D3 datasheets. However, experiments
    // on a Nelson device (Amlogic S905D3) show that the value is the same as
    // other devices.
    kTimingControllerClock = 15,
  };
  DEF_ENUM_FIELD(HdmiTxPixelClockSource, 19, 16, hdmi_tx_pixel_clock_selection);

  DEF_RSVDZ_FIELD(15, 11);

  enum class HdmiTxSystemClockSource : uint32_t {
    kExternalOscillator24Mhz = 0,
    kFixed500Mhz = 1,
    kFixed666Mhz = 2,
    kFixed400Mhz = 3,
  };
  DEF_ENUM_FIELD(HdmiTxSystemClockSource, 10, 9, hdmi_tx_system_clock_selection);

  DEF_BIT(8, hdmi_tx_system_clock_enabled);

  DEF_RSVDZ_BIT(7);

  // It's preferred to use {Get,Set}HdmiTxSystemClockDivider() to get / set the
  // divider value.
  DEF_FIELD(6, 0, hdmi_tx_system_clock_divider_minus_one);
  static constexpr int kMinHdmiTxSystemClockDivider = 1;
  static constexpr int kMaxHdmiTxSystemClockDivider = [] {
    constexpr int kFieldLow = 0;
    constexpr int kFieldHigh = 6;
    return 1 << (kFieldHigh - kFieldLow + 1);
  }();

  HdmiClockControl& SetHdmiTxSystemClockDivider(int divider) {
    ZX_DEBUG_ASSERT(divider >= kMinHdmiTxSystemClockDivider);
    ZX_DEBUG_ASSERT(divider <= kMaxHdmiTxSystemClockDivider);
    return set_hdmi_tx_system_clock_divider_minus_one(divider - 1);
  }

  int GetHdmiTxSystemClockDivider() const { return hdmi_tx_system_clock_divider_minus_one() + 1; }

  static auto Get() {
    constexpr uint32_t kRegAddr = 0x73 * sizeof(uint32_t);
    return hwreg::RegisterAddr<HdmiClockControl>(kRegAddr);
  }
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_CLOCK_REGS_H_
