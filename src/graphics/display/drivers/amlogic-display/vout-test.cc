// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/vout.h"

#include <gtest/gtest.h>

namespace amlogic_display {

namespace {
TEST(Vout, UnsupportedHdmi) {
  auto vout = std::make_unique<Vout>();

  // PowerOff and PowerOn should fail if Hdmi is not initialized.
  EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, vout->PowerOff().status_value());
  EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, vout->PowerOn().status_value());
}

}  // namespace

}  // namespace amlogic_display
