// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_DISPLAY_COLOR_CONVERTER_H_
#define SRC_UI_SCENIC_LIB_DISPLAY_COLOR_CONVERTER_H_

#include <fuchsia/ui/display/color/cpp/fidl.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/sys/cpp/component_context.h>

namespace scenic_impl::display {

using SetColorConversionFunc = fit::function<void(const fidl::Array<float, 9>& coefficients,
                                                  const fidl::Array<float, 3>& preoffsets,
                                                  const fidl::Array<float, 3>& postoffsets)>;
using SetMinimumRgbFunc = fit::function<bool(uint8_t minimum_rgb)>;

// Backend for the ColorConverter FIDL interface.
class ColorConverter : public fuchsia::ui::display::color::Converter {
 public:
  ColorConverter(sys::ComponentContext* app_context,
                 SetColorConversionFunc set_color_conversion_values,
                 SetMinimumRgbFunc set_minimum_rgb);

  // |fuchsia.ui.display.color.Converter|
  void SetValues(fuchsia::ui::display::color::ConversionProperties properties,
                 SetValuesCallback callback) override;

  // |fuchsia.ui.display.color.Converter|
  void SetMinimumRgb(uint8_t minimum_rgb, SetMinimumRgbCallback callback) override;

 private:
  fidl::BindingSet<fuchsia::ui::display::color::Converter> bindings_;

  const SetColorConversionFunc set_color_conversion_values_;
  const SetMinimumRgbFunc set_minimum_rgb_;
};

}  // namespace scenic_impl::display

#endif  // SRC_UI_SCENIC_LIB_DISPLAY_COLOR_CONVERTER_H_
