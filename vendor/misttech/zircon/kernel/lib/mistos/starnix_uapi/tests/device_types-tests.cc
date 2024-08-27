// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix_uapi/device_type.h>

#include <zxtest/zxtest.h>

using namespace starnix_uapi;

namespace {

TEST(DeviceType, test_device_type) {
  auto dev = DeviceType::New(21, 17);
  ASSERT_EQ(dev.major(), 21);
  ASSERT_EQ(dev.minor(), 17);

  dev = DeviceType::New(0x83af83fe, 0xf98ecba1);
  ASSERT_EQ(dev.major(), 0x83af83fe);
  ASSERT_EQ(dev.minor(), 0xf98ecba1);
}

}  // namespace
