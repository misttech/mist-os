// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "test-helpers.h"

TEST(SysfsPowerStateTest, Success) { ASSERT_NO_FATAL_FAILURE(DoTest("success", "fail", true)); }
