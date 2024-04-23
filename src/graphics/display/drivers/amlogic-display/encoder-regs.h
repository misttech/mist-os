// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_ENCODER_REGS_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_ENCODER_REGS_H_

#include <zircon/assert.h>

#include <cstdint>

#include <hwreg/bitfields.h>

namespace amlogic_display {

// VENC_VIDEO_TST_EN
//
// S912 Datasheet, Section 27.5 "CVBS and LCD", page 377.
class EncoderBuiltInSelfTestEnabled
    : public hwreg::RegisterBase<EncoderBuiltInSelfTestEnabled, uint32_t> {
 public:
  static hwreg::RegisterAddr<EncoderBuiltInSelfTestEnabled> Get() {
    return {0x1b70 * sizeof(uint32_t)};
  }

  // These bits are undocumented on Amlogic datasheets.
  // We model these bits as reserved must be zero because Amlogic-provided code
  // unconditionally sets them to zero.
  DEF_RSVDZ_FIELD(31, 1);

  // If true, the encoder built-in self-test (BIST) mode is enabled, and the
  // encoder outputs predefined patterns as specified in the
  // EncoderBuiltInSelfTestModeSelection register.
  DEF_BIT(0, enabled);
};

enum class EncoderBuiltInSelfTestMode : uint8_t {
  // Outputs a fixed color specified by the following registers:
  // - EncoderBuiltInSelfTestFixedColorLuminance
  // - EncoderBuiltInSelfTestFixedColorChrominanceBlue
  // - EncoderBuiltInSelfTestFixedColorChrominanceRed
  kFixedColor = 0,

  // Outputs color bars with 100% and 75% luminance (intensity), also known as
  // 100/75 color bars.
  kColorBar = 1,

  // Outputs thin horizontal and vertical lines on the screen.
  kThinLines = 2,

  // Outputs a grid of white dots on the display.
  kDotGrid = 3,
};

// VENC_VIDEO_TST_MDSEL
//
// S912 Datasheet, Section 27.5 "CVBS and LCD", pages 377-378.
class EncoderBuiltInSelfTestModeSelection
    : public hwreg::RegisterBase<EncoderBuiltInSelfTestModeSelection, uint32_t> {
 public:
  static hwreg::RegisterAddr<EncoderBuiltInSelfTestModeSelection> Get() {
    return {0x1b71 * sizeof(uint32_t)};
  }

  // These bits are undocumented on Amlogic datasheets.
  // We model these bits as reserved must be zero because Amlogic-provided code
  // unconditionally sets them to zero.
  DEF_RSVDZ_FIELD(31, 8);

  DEF_ENUM_FIELD(EncoderBuiltInSelfTestMode, 7, 0, mode);
};

class EncoderBuiltInSelfTestFixedColorLuminance
    : public hwreg::RegisterBase<EncoderBuiltInSelfTestFixedColorLuminance, uint32_t> {
 public:
  static hwreg::RegisterAddr<EncoderBuiltInSelfTestFixedColorLuminance> Get() {
    return {0x1b72 * sizeof(uint32_t)};
  }

  // These bits are undocumented on Amlogic datasheets.
  // We model these bits as reserved must be zero because Amlogic-provided code
  // unconditionally sets them to zero.
  DEF_RSVDZ_FIELD(31, 10);

  DEF_FIELD(9, 0, luminance);
};

// VENC_VIDEO_TST_CB
//
// S912 Datasheet, Section 27.5 "CVBS and LCD", pages 378.
class EncoderBuiltInSelfTestFixedColorChrominanceBlue
    : public hwreg::RegisterBase<EncoderBuiltInSelfTestFixedColorChrominanceBlue, uint32_t> {
 public:
  static hwreg::RegisterAddr<EncoderBuiltInSelfTestFixedColorChrominanceBlue> Get() {
    return {0x1b73 * sizeof(uint32_t)};
  }

  // These bits are undocumented on Amlogic datasheets.
  // We model these bits as reserved must be zero because Amlogic-provided code
  // unconditionally sets them to zero.
  DEF_RSVDZ_FIELD(31, 10);

  DEF_FIELD(9, 0, chrominance_blue);
};

// VENC_VIDEO_TST_CR
//
// S912 Datasheet, Section 27.5 "CVBS and LCD", pages 378.
class EncoderBuiltInSelfTestFixedColorChrominanceRed
    : public hwreg::RegisterBase<EncoderBuiltInSelfTestFixedColorChrominanceRed, uint32_t> {
 public:
  static hwreg::RegisterAddr<EncoderBuiltInSelfTestFixedColorChrominanceRed> Get() {
    return {0x1b74 * sizeof(uint32_t)};
  }

  // These bits are undocumented on Amlogic datasheets.
  // We model these bits as reserved must be zero because Amlogic-provided code
  // unconditionally sets them to zero.
  DEF_RSVDZ_FIELD(31, 10);

  DEF_FIELD(9, 0, chrominance_red);
};

// ENCP_VIDEO_MODE_ADV
//
// S912 Datasheet, Section 27.5 "CVBS and LCD", page 382.
class HdmiEncoderAdvancedModeConfig
    : public hwreg::RegisterBase<HdmiEncoderAdvancedModeConfig, uint32_t> {
 public:
  // Selection of the downsampling multiplier (reciprocal of the downsampling
  // ratio) from the VIU to the VIU FIFO (VFIFO).
  //
  // A downsampling multiplier of N means that the VIU FIFO only takes one of
  // every N pixels from the VIU's output.
  enum class ViuFifoDownsamplingMultiplierSelection : uint8_t {
    k1 = 0,
    k2 = 1,
    k4 = 2,
    k8 = 3,
  };
  static hwreg::RegisterAddr<HdmiEncoderAdvancedModeConfig> Get() {
    return {0x1b8e * sizeof(uint32_t)};
  }

  // This field is undocumented on Amlogic datasheets.
  // Amlogic-provided code directly writes 0 to this field regardless of its
  // original value, so we can believe that zero is a safe setting for this
  // field.
  DEF_RSVDZ_FIELD(31, 16);

  DEF_FIELD(15, 14, sp_timing_control);

  DEF_BIT(13, cr_bypasses_limiter);
  DEF_BIT(12, cb_bypasses_limiter);
  DEF_BIT(11, y_bypasses_limiter);
  DEF_BIT(10, gamma_rgb_input_selection);

  DEF_RSVDZ_FIELD(9, 8);

  // True iff the hue adjustment matrix (controlled by ENCP_VIDEO_MATRIX_CB
  // and ENCP_VIDEO_MATRIX_CR) is enabled.
  DEF_BIT(7, hue_matrix_enabled);

  // Bits 6-4 are related to YPbPr (analog output) which seem to be unused
  // for the HDMI encoder.
  DEF_BIT(6, pb_pr_swapped);
  DEF_BIT(5, pb_pr_hsync_enabled);
  DEF_BIT(4, ypbpr_gain_as_hdtv_type);

  // True iff the VIU FIFO (VFIFO) which samples the VIU output pixels is
  // enabled.
  DEF_BIT(3, viu_fifo_enabled);

  // It's preferred to use the `viu_fifo_downsampling_multiplier()` and
  // `set_viu_fifo_downsampling_multiplier()` helper methods.
  DEF_ENUM_FIELD(ViuFifoDownsamplingMultiplierSelection, 2, 0,
                 viu_fifo_downsampling_multiplier_selection);

  int viu_fifo_downsampling_multiplier() const {
    switch (viu_fifo_downsampling_multiplier_selection()) {
      case ViuFifoDownsamplingMultiplierSelection::k1:
        return 1;
      case ViuFifoDownsamplingMultiplierSelection::k2:
        return 2;
      case ViuFifoDownsamplingMultiplierSelection::k4:
        return 4;
      case ViuFifoDownsamplingMultiplierSelection::k8:
        return 8;
    }
  }

  // `multiplier` must be 1, 2, 4 or 8.
  HdmiEncoderAdvancedModeConfig& set_viu_fifo_downsampling_multiplier(int multiplier) {
    switch (multiplier) {
      case 1:
        return set_viu_fifo_downsampling_multiplier_selection(
            ViuFifoDownsamplingMultiplierSelection::k1);
      case 2:
        return set_viu_fifo_downsampling_multiplier_selection(
            ViuFifoDownsamplingMultiplierSelection::k2);
      case 4:
        return set_viu_fifo_downsampling_multiplier_selection(
            ViuFifoDownsamplingMultiplierSelection::k4);
      case 8:
        return set_viu_fifo_downsampling_multiplier_selection(
            ViuFifoDownsamplingMultiplierSelection::k8);
    }
    ZX_DEBUG_ASSERT_MSG(false, "Invalid downsampling multiplier: %d", multiplier);
    return *this;
  }
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_ENCODER_REGS_H_
