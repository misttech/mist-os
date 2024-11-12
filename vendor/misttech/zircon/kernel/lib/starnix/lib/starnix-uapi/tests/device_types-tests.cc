// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

namespace {

using starnix_uapi::DeviceType;

bool test_device_type() {
  BEGIN_TEST;

  auto dev = DeviceType::New(21, 17);
  ASSERT_EQ(21u, dev.major());
  ASSERT_EQ(17u, dev.minor());

  dev = DeviceType::New(0x83af83fe, 0xf98ecba1);
  ASSERT_EQ(0x83af83fe, dev.major());
  ASSERT_EQ(0xf98ecba1, dev.minor());

  END_TEST;
}
}  // namespace

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_uapi_devicetype)
UNITTEST("test device type", unit_testing::test_device_type)
UNITTEST_END_TESTCASE(starnix_uapi_devicetype, "starnix_uapi_devicetype", "Tests Device Type")
