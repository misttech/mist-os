// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {
namespace {

using starnix_uapi::OpenFlagsImpl;

bool test_access() {
  BEGIN_TEST;

  auto read_only = OpenFlagsImpl::from_bits_truncate(O_RDONLY);
  EXPECT_TRUE(read_only.can_read());
  EXPECT_FALSE(read_only.can_write());

  auto write_only = OpenFlagsImpl::from_bits_truncate(O_WRONLY);
  EXPECT_FALSE(write_only.can_read());
  EXPECT_TRUE(write_only.can_write());

  auto read_write = OpenFlagsImpl::from_bits_truncate(O_RDWR);
  EXPECT_TRUE(read_write.can_read());
  EXPECT_TRUE(read_write.can_write());

  EXPECT_TRUE(static_cast<uint64_t>(OpenFlagsImpl::EnumType::ACCESS_MASK) == 3ul);

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_uapi_openflags)
UNITTEST("test access", unit_testing::test_access)
UNITTEST_END_TESTCASE(starnix_uapi_openflags, "starnix_uapi_openflags", "Tests Open Flags")
