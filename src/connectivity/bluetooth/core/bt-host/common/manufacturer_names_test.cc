// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/manufacturer_names.h"

#include <gtest/gtest.h>

namespace bt {
namespace {

TEST(ManufacturerNamesTest, NameIsHexString) {
  EXPECT_EQ("0x0000", GetManufacturerName(0x0000));
  EXPECT_EQ("0x1234", GetManufacturerName(0x1234));
  EXPECT_EQ("0x9999", GetManufacturerName(0x9999));
  EXPECT_EQ("0x0123", GetManufacturerName(0x0123));
  EXPECT_EQ("0x0fff", GetManufacturerName(0x0fff));
  EXPECT_EQ("0x0023", GetManufacturerName(0x0023));
  EXPECT_EQ("0xffff", GetManufacturerName(0xffff));
  EXPECT_EQ("0x0abc", GetManufacturerName(0x0abc));
}

}  // namespace
}  // namespace bt
