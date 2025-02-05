// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/color_converter.h"

#include <algorithm>
#include <cmath>

#include <sdk/lib/syslog/cpp/macros.h>

#include "src/ui/scenic/lib/utils/helpers.h"

namespace scenic_impl::display {

namespace {

template <typename T>
bool AreValid(const T& values) {
  return std::all_of(begin(values), end(values),
                     [](const auto& value) { return std::isfinite(value); });
}

}  // namespace

ColorConverter::ColorConverter(sys::ComponentContext* app_context,
                               SetColorConversionFunc set_color_conversion_values,
                               SetMinimumRgbFunc set_minimum_rgb)
    : set_color_conversion_values_(std::move(set_color_conversion_values)),
      set_minimum_rgb_(std::move(set_minimum_rgb)) {
  FX_DCHECK(app_context);
  FX_DCHECK(set_color_conversion_values_);
  FX_DCHECK(set_minimum_rgb_);
  app_context->outgoing()->AddPublicService(bindings_.GetHandler(this));
}

void ColorConverter::SetValues(fuchsia::ui::display::color::ConversionProperties properties,
                               SetValuesCallback callback) {
  const auto coefficients = properties.has_coefficients()
                                ? properties.coefficients()
                                : std::array<float, 9>{1, 0, 0, 0, 1, 0, 0, 0, 1};
  const auto preoffsets =
      properties.has_preoffsets() ? properties.preoffsets() : std::array<float, 3>{0, 0, 0};
  const auto postoffsets =
      properties.has_postoffsets() ? properties.postoffsets() : std::array<float, 3>{0, 0, 0};

  if (!AreValid(coefficients) || !AreValid(preoffsets) || !AreValid(postoffsets)) {
    const std::string& coefficients_str = utils::GetArrayString("Coefficients", coefficients);
    const std::string& preoffsets_str = utils::GetArrayString("Preoffsets", preoffsets);
    const std::string& postoffsets_str = utils::GetArrayString("Postoffsets", postoffsets);
    FX_LOGS(ERROR) << "Invalid Color Conversion Parameter Values: \n"
                   << coefficients_str << preoffsets_str << postoffsets_str;
    callback(ZX_ERR_INVALID_ARGS);
    return;
  }

  fidl::Arena arena;
  set_color_conversion_values_(fidl::ToWire(arena, coefficients), fidl::ToWire(arena, preoffsets),
                               fidl::ToWire(arena, postoffsets));
  callback(ZX_OK);
}

void ColorConverter::SetMinimumRgb(uint8_t minimum_rgb, SetMinimumRgbCallback callback) {
  callback(set_minimum_rgb_(minimum_rgb));
}

}  // namespace scenic_impl::display
