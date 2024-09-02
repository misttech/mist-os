// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/unittest/unittest.h>

#include <ktl/string_view.h>

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
  ASSERT_BYTES_EQ(reinterpret_cast<const uint8_t*>(expected.data()),
                  reinterpret_cast<const uint8_t*>(actual.data()), expected.size());

  p = PathBuilder();
  actual = p.build_relative();
  expected = ktl::string_view("");
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_BYTES_EQ(reinterpret_cast<const uint8_t*>(expected.data()),
                  reinterpret_cast<const uint8_t*>(actual.data()), expected.size());

  p = PathBuilder();
  p.prepend_element("foo");
  actual = p.build_absolute();
  expected = ktl::string_view("/foo");
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_BYTES_EQ(reinterpret_cast<const uint8_t*>(expected.data()),
                  reinterpret_cast<const uint8_t*>(actual.data()), expected.size());

  p = PathBuilder();
  p.prepend_element("foo");
  actual = p.build_relative();
  expected = ktl::string_view("foo");
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_BYTES_EQ(reinterpret_cast<const uint8_t*>(expected.data()),
                  reinterpret_cast<const uint8_t*>(actual.data()), expected.size());

  p = PathBuilder();
  p.prepend_element("foo");
  p.prepend_element("bar");
  actual = p.build_absolute();
  expected = ktl::string_view("/bar/foo");
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_BYTES_EQ(reinterpret_cast<const uint8_t*>(expected.data()),
                  reinterpret_cast<const uint8_t*>(actual.data()), expected.size());

  p = PathBuilder();
  p.prepend_element("foo");
  p.prepend_element("1234567890123456789012345678901234567890");
  p.prepend_element("bar");
  actual = p.build_absolute();
  expected = ktl::string_view("/bar/1234567890123456789012345678901234567890/foo");
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_BYTES_EQ(reinterpret_cast<const uint8_t*>(expected.data()),
                  reinterpret_cast<const uint8_t*>(actual.data()), expected.size());

  END_TEST;
}

UNITTEST_START_TESTCASE(starnix_path_builder)
UNITTEST("test path builder", test_path_builder)
UNITTEST_END_TESTCASE(starnix_path_builder, "starnix_path_builder", "Tests for PathBuilder")
