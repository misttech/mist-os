// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/runtime/testing/loop_fixture/test_loop_fixture.h"

class FixtureTest : public ::gtest::DriverTestLoopFixture {};

TEST_F(FixtureTest, BuildTest) {}
