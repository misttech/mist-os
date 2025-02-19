// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {
namespace {

using starnix_uapi::FileMode;

bool test_file_mode_from_string() {
  BEGIN_TEST;

  ASSERT_EQ(FileMode(0123).bits(), FileMode::from_string("0123").value().bits());
  ASSERT_TRUE(FileMode::from_string("123").is_error());
  ASSERT_TRUE(FileMode::from_string("\x80").is_error());
  ASSERT_TRUE(FileMode::from_string("0999").is_error());

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_uapi_filemode)
UNITTEST("test file mode from string", unit_testing::test_file_mode_from_string)
UNITTEST_END_TESTCASE(starnix_uapi_filemode, "starnix_uapi_filemode", "Tests File Mode")
