// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/testing/client-utils/display.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <zircon/syscalls.h>

#include <array>
#include <cmath>
#include <cstdio>

#include "src/graphics/display/lib/api-types-cpp/display-id.h"

namespace fhd = fuchsia_hardware_display;

namespace testing {
namespace display {

Display::Display(const fhd::wire::Info& info) {
  id_ = ::display::ToDisplayId(info.id);

  for (fuchsia_images2::wire::PixelFormat pixel_format : info.pixel_format) {
    ZX_ASSERT(!pixel_format.IsUnknown());
    pixel_formats_.push_back(pixel_format);
  }

  auto mode = reinterpret_cast<const fhd::wire::Mode*>(info.modes.data());
  for (unsigned i = 0; i < info.modes.count(); i++) {
    modes_.push_back(mode[i]);
  }

  auto cursors = reinterpret_cast<const fhd::wire::CursorInfo*>(info.cursor_configs.data());
  for (unsigned i = 0; i < info.cursor_configs.count(); i++) {
    cursors_.push_back(cursors[i]);
  }

  manufacturer_name_ = fbl::String(info.manufacturer_name.data());
  monitor_name_ = fbl::String(info.monitor_name.data());
  monitor_serial_ = fbl::String(info.monitor_serial.data());

  horizontal_size_mm_ = info.horizontal_size_mm;
  vertical_size_mm_ = info.vertical_size_mm;
  using_fallback_sizes_ = info.using_fallback_size;
}

void Display::Dump() {
  printf("Display id = %ld\n", id_.value());
  printf("\tManufacturer name = \"%s\"\n", manufacturer_name_.c_str());
  printf("\tMonitor name = \"%s\"\n", monitor_name_.c_str());
  printf("\tMonitor serial = \"%s\"\n", monitor_serial_.c_str());

  printf("\tSupported pixel formats:\n");
  for (unsigned i = 0; i < pixel_formats_.size(); i++) {
    printf("\t\t%d\t: %8u\n", i, static_cast<uint32_t>(pixel_formats_[i]));
  }

  printf("\n\tSupported display modes:\n");
  for (unsigned i = 0; i < modes_.size(); i++) {
    printf("\t\t%d\t: %dx%d\t%d.%02d\n", i, modes_[i].horizontal_resolution,
           modes_[i].vertical_resolution, modes_[i].refresh_rate_e2 / 100,
           modes_[i].refresh_rate_e2 % 100);
  }

  printf("\n\tSupported cursor modes:\n");
  for (unsigned i = 0; i < cursors_.size(); i++) {
    printf("\t\t%d\t: %dx%d\t%08x\n", i, cursors_[i].width, cursors_[i].height,
           static_cast<uint32_t>(cursors_[i].pixel_format));
  }

  printf("\n\t%s Physical dimension in millimeters:\n",
         using_fallback_sizes_ ? "[Best Guess / Fallback]" : "");
  printf("\t\tHorizontal size = %d mm\n", horizontal_size_mm_);
  printf("\t\tVertical size = %d mm\n", vertical_size_mm_);
  printf("\n");
}

void Display::Init(const fidl::WireSyncClient<fhd::Coordinator>& dc,
                   ColorCorrectionArgs color_correction_args) {
  fhd::wire::DisplayId fidl_display_id = ToFidlDisplayId(id_);
  if (mode_idx_ != 0) {
    ZX_ASSERT(dc->SetDisplayMode(fidl_display_id, modes_[mode_idx_]).ok());
  }

  if (grayscale_) {
    ::fidl::Array<float, 3> preoffsets = {nanf("pre"), 0, 0};
    ::fidl::Array<float, 3> postoffsets = {nanf("post"), 0, 0};
    ::fidl::Array<float, 9> grayscale = {
        .2126f, .7152f, .0722f, .2126f, .7152f, .0722f, .2126f, .7152f, .0722f,
    };
    ZX_ASSERT(
        dc->SetDisplayColorConversion(fidl_display_id, preoffsets, grayscale, postoffsets).ok());
  } else if (apply_color_correction_) {
    ::fidl::Array<float, 3> preoffsets = color_correction_args.preoffsets;
    ::fidl::Array<float, 3> postoffsets = color_correction_args.postoffsets;
    ::fidl::Array<float, 9> grayscale = color_correction_args.coeff;
    ZX_ASSERT(
        dc->SetDisplayColorConversion(fidl_display_id, preoffsets, grayscale, postoffsets).ok());
  }
}

}  // namespace display
}  // namespace testing
