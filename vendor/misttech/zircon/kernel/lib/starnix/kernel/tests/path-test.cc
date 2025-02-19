// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/util/testing/unittest.h>
#include <lib/unittest/unittest.h>

#include <ktl/string_view.h>

namespace unit_testing {

using namespace starnix;

bool test_path_builder() {
  BEGIN_TEST;
  FsString expected;
  FsString actual;
  PathBuilder p;

  p = PathBuilder();
  actual = p.build_absolute();
  expected = ktl::string_view("/");
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_STREQ(expected, actual);

  p = PathBuilder();
  actual = p.build_relative();
  expected = ktl::string_view("");
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_STREQ(expected, actual);

  p = PathBuilder();
  p.prepend_element("foo");
  actual = p.build_absolute();
  expected = ktl::string_view("/foo");
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_STREQ(expected, actual);

  p = PathBuilder();
  p.prepend_element("foo");
  actual = p.build_relative();
  expected = ktl::string_view("foo");
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_STREQ(expected, actual);

  p = PathBuilder();
  p.prepend_element("foo");
  p.prepend_element("bar");
  actual = p.build_absolute();
  expected = ktl::string_view("/bar/foo");
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_STREQ(expected, actual);

  p = PathBuilder();
  p.prepend_element("foo");
  p.prepend_element("1234567890123456789012345678901234567890");
  p.prepend_element("bar");
  actual = p.build_absolute();
  expected = ktl::string_view("/bar/1234567890123456789012345678901234567890/foo");
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_STREQ(expected, actual);

  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_path_builder)
UNITTEST("test path builder", unit_testing::test_path_builder)
UNITTEST_END_TESTCASE(starnix_path_builder, "starnix_path_builder", "Tests for PathBuilder")
