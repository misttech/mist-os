// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/src/recursion_detector.h"

namespace fidlc {
namespace {

// These objects exist solely so that the test code below can get a pointer to their address,
// which is passed into RecursionDetector.Enter().
int object;
int object2;

TEST(RecursionDetectorTests, EnterSameObjectTwiceResultsInNoGuard) {
  RecursionDetector rd;

  auto guard = rd.Enter(&object);
  ASSERT_TRUE(guard.has_value());

  auto guard2 = rd.Enter(&object2);
  ASSERT_TRUE(guard2.has_value());

  auto no_guard = rd.Enter(&object);
  ASSERT_FALSE(no_guard.has_value());
}

TEST(RecursionDetectorTests, GuardObjectPopsSeenObjectsOnScopeExit) {
  RecursionDetector rd;

  auto guard = rd.Enter(&object);
  ASSERT_TRUE(guard.has_value());

  {
    auto guard2 = rd.Enter(&object2);
    ASSERT_TRUE(guard2.has_value());
  }

  {
    auto new_guard2 = rd.Enter(&object2);
    ASSERT_TRUE(new_guard2.has_value());
  }
}

}  // namespace
}  // namespace fidlc
