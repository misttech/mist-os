// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/starnix/tests/syscalls/test_helper.h"

#include <gtest/gtest.h>

namespace {

TEST(TestHelperTest, DetectFailingChildren) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([] { FAIL() << "Expected failure"; });

  EXPECT_FALSE(helper.WaitForChildren());
}

}  // namespace
