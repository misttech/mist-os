// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/utils/errors.h"

#include <gtest/gtest.h>

namespace forensics {
namespace {

TEST(ErrorsTest, ValidUtf8String) {
  const ErrorOrString value("value1");

  ASSERT_TRUE(value.HasValue());
  EXPECT_EQ(value.Value(), "value1");
}

TEST(ErrorsTest, InvalidUtf8String) {
  const ErrorOrString value("\xC0\x80");

  ASSERT_FALSE(value.HasValue());
  EXPECT_EQ(value.Error(), Error::kInvalidFormat);
}

}  // namespace
}  // namespace forensics
