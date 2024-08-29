// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix_uapi/file_mode.h>

#include <zxtest/zxtest.h>

using namespace starnix_uapi;

namespace {

TEST(FileMode, test_file_mode_from_string) {
  ASSERT_EQ(FileMode(0123), FileMode::from_string("0123").value());
  ASSERT_TRUE(FileMode::from_string("123").is_error());
  ASSERT_TRUE(FileMode::from_string("\x80").is_error());
  ASSERT_TRUE(FileMode::from_string("0999").is_error());
}

}  // namespace
