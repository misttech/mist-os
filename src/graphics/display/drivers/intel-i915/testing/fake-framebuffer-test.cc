// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-i915/testing/fake-framebuffer.h"

#include <lib/zbi-format/graphics.h>
#include <zircon/syscalls.h>

#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

namespace {

TEST(FakeFramebuffer, Error) {
  fake_framebuffer::SetFramebuffer({
      .status = ZX_ERR_NOT_FOUND,
  });

  uint32_t format, width, height, stride;
  zx_handle_t resource = 0x0a0b0c0d;
  zx_status_t status = zx_framebuffer_get_info(resource, &format, &width, &height, &stride);
  EXPECT_STATUS(status, ZX_ERR_NOT_FOUND);
}

TEST(FakeFramebuffer, Ok) {
  constexpr fake_framebuffer::Framebuffer kExpectedFramebuffer = {
      .status = ZX_OK,
      .format = ZBI_PIXEL_FORMAT_RGB_888,
      .width = 0x05060708,
      .height = 0x090a0b0c,
      .stride = 0x0d0e0f10,
  };
  fake_framebuffer::SetFramebuffer(kExpectedFramebuffer);

  uint32_t format, width, height, stride;
  zx_handle_t resource = 0x01020304;
  zx_status_t status = zx_framebuffer_get_info(resource, &format, &width, &height, &stride);
  EXPECT_OK(status);
  EXPECT_EQ(format, kExpectedFramebuffer.format);
  EXPECT_EQ(width, kExpectedFramebuffer.width);
  EXPECT_EQ(height, kExpectedFramebuffer.height);
  EXPECT_EQ(stride, kExpectedFramebuffer.stride);
}

}  // namespace
