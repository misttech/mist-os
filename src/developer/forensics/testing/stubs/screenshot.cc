// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/screenshot.h"

#include <fuchsia/images/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/vmo.h>

#include <cstdint>

namespace forensics::stubs {
namespace {

struct BGRA {
  uint8_t b;
  uint8_t g;
  uint8_t r;
  uint8_t a;
};

}  // namespace

fuchsia::ui::composition::ScreenshotTakeResponse CreateCheckerboardScreenshot(
    const uint32_t image_dim_in_px) {
  const uint32_t height = image_dim_in_px;
  const uint32_t width = image_dim_in_px;
  const size_t block_size = 10;
  const uint8_t black = 0;
  const uint8_t white = 0xff;

  const size_t size_in_bytes = image_dim_in_px * image_dim_in_px * sizeof(BGRA);
  auto ptr = std::make_unique<uint8_t[]>(size_in_bytes);
  BGRA* pixels = reinterpret_cast<BGRA*>(ptr.get());

  // We go pixel by pixel, row by row. |y| tracks the row and |x| the column.
  //
  // We compute in which |block_size| x |block_size| block the pixel is to determine the color
  // (black or white). |block_y| tracks the "block" row and |block_x| the "block" column.
  for (size_t y = 0; y < height; ++y) {
    size_t block_y = y / block_size;
    for (size_t x = 0; x < width; ++x) {
      size_t block_x = x / block_size;
      uint8_t block_color = (block_x + block_y) % 2 ? black : white;
      size_t index = y * width + x;
      auto& p = pixels[index];
      p.r = p.g = p.b = block_color;
      p.a = 255;
    }
  }

  fuchsia::ui::composition::ScreenshotTakeResponse screenshot;
  FX_CHECK(zx::vmo::create(size_in_bytes, 0u, screenshot.mutable_vmo()) == ZX_OK);
  FX_CHECK(screenshot.mutable_vmo()->write(ptr.get(), 0u, size_in_bytes) == ZX_OK);
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
