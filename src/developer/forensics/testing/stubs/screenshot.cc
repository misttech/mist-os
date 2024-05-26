// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/screenshot.h"

#include <fuchsia/images/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/vmo.h>

#include <cstdint>

namespace forensics::stubs {

fuchsia::ui::composition::ScreenshotTakeResponse CreateFakePNGScreenshot(
    const uint32_t image_dim_in_px) {
  const uint32_t height = image_dim_in_px;
  const uint32_t width = image_dim_in_px;

  fuchsia::ui::composition::ScreenshotTakeResponse screenshot;
  FX_CHECK(zx::vmo::create(static_cast<uint64_t>(width * height * 4), 0u,
                           screenshot.mutable_vmo()) == ZX_OK);

  uint64_t size_in_bytes;
  screenshot.vmo().get_size(&size_in_bytes);

  // Actual pixel values don't matter, we just want to check for equality.
  std::vector<uint8_t> fake_vals;
  fake_vals.resize(size_in_bytes);
  for (size_t i = 0; i < size_in_bytes; i++) {
    fake_vals[i] = static_cast<uint8_t>(i);
  }

  FX_CHECK(screenshot.mutable_vmo()->write(fake_vals.data(), 0u, size_in_bytes) == ZX_OK);
  screenshot.set_size({.width = width, .height = height});
  return screenshot;
}

Screenshot::~Screenshot() {
  FX_CHECK(screenshot_take_responses_.empty())
      << "server still has " << screenshot_take_responses_.size() << " screenshot responses";
}

void Screenshot::Take(fuchsia::ui::composition::ScreenshotTakeRequest request,
                      TakeCallback callback) {
  FX_CHECK(!screenshot_take_responses_.empty())
      << "You need to set up Screenshot::Take() responses first.";
  auto response = std::move(screenshot_take_responses_.front());
  screenshot_take_responses_.pop_front();
  callback(std::move(response));
}

}  // namespace forensics::stubs
