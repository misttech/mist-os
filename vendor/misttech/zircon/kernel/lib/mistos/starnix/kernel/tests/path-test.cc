// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/vfs/path.h>

#include <zxtest/zxtest.h>

namespace {

using namespace starnix;

TEST(Path, test_path_builder) {
  PathBuilder p;
  ASSERT_EQ("/", p.build_absolute());

  p = PathBuilder();
  ASSERT_EQ("", p.build_relative());

  p = PathBuilder();
  p.prepend_element("foo");
  ASSERT_EQ("/foo", p.build_absolute());

  p = PathBuilder();
  p.prepend_element("foo");
  ASSERT_EQ("foo", p.build_relative());

  p = PathBuilder();
  p.prepend_element("foo");
  p.prepend_element("bar");
  ASSERT_EQ("/bar/foo", p.build_absolute());

  p = PathBuilder();
  p.prepend_element("foo");
  p.prepend_element("1234567890123456789012345678901234567890");
  p.prepend_element("bar");
  ASSERT_EQ("/bar/1234567890123456789012345678901234567890/foo", p.build_absolute());
}

}  // namespace
