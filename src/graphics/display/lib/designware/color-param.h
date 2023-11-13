// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_COLOR_PARAM_H_
#define SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_COLOR_PARAM_H_

#include <fidl/fuchsia.hardware.hdmi/cpp/wire.h>
#include <zircon/assert.h>

#include <cinttypes>

namespace hdmi_dw {

// Equivalent to the FIDL type [`fuchsia.hardware.hdmi/ColorFormat`].
//
// TODO(fxbug.dev/136194): Revise the naming of the enum type and member names.
// Add proper documentatation.
enum class ColorFormat : uint8_t {
  kCfRgb = 0,
  kCf444 = 1,
};

constexpr ColorFormat ToColorFormat(fuchsia_hardware_hdmi::wire::ColorFormat fidl_color_format) {
  switch (fidl_color_format) {
    case fuchsia_hardware_hdmi::ColorFormat::kCfRgb:
      return ColorFormat::kCfRgb;
    case fuchsia_hardware_hdmi::ColorFormat::kCf444:
      return ColorFormat::kCf444;
  }
  ZX_DEBUG_ASSERT_MSG(false, "Invalid ColorFormat: %" PRIu8,
                      static_cast<uint8_t>(fidl_color_format));
}

// Equivalent to the FIDL type [`fuchsia.hardware.hdmi/ColorDepth`].
//
// TODO(fxbug.dev/136194): Revise the naming of the enum type and member names.
// Add proper documentatation.
enum class ColorDepth : uint8_t {
  kCd24B = 4,
  kCd30B = 5,
  kCd36B = 6,
  kCd48B = 7,
};

constexpr ColorDepth ToColorDepth(fuchsia_hardware_hdmi::wire::ColorDepth fidl_color_depth) {
  switch (fidl_color_depth) {
    case fuchsia_hardware_hdmi::ColorDepth::kCd24B:
      return ColorDepth::kCd24B;
    case fuchsia_hardware_hdmi::ColorDepth::kCd30B:
      return ColorDepth::kCd30B;
    case fuchsia_hardware_hdmi::ColorDepth::kCd36B:
      return ColorDepth::kCd36B;
    case fuchsia_hardware_hdmi::ColorDepth::kCd48B:
      return ColorDepth::kCd48B;
  }
  ZX_DEBUG_ASSERT_MSG(false, "Invalid ColorDepth: %" PRIu8, static_cast<uint8_t>(fidl_color_depth));
}

// Color configuration of the HDMI transmitter.
// Equivalent to the FIDL type [`fuchsia.hardware.hdmi/ColorParam`].
//
// TODO(fxbug.dev/136194): Revise the naming of the enum type and member names.
// Add proper documentatation.
struct ColorParam {
  ColorFormat input_color_format;
  ColorFormat output_color_format;
  ColorDepth color_depth;
};

constexpr ColorParam ToColorParam(fuchsia_hardware_hdmi::wire::ColorParam fidl_color_param) {
  return {
      .input_color_format = ToColorFormat(fidl_color_param.input_color_format),
      .output_color_format = ToColorFormat(fidl_color_param.output_color_format),
      .color_depth = ToColorDepth(fidl_color_param.color_depth),
  };
}

}  // namespace hdmi_dw

#endif  // SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_COLOR_PARAM_H_
