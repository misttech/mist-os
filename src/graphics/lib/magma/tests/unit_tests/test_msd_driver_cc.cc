// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "msd.h"

TEST(MsdDriver, CreateAndDestroy) {
  auto msd_driver = msd::Driver::Create();
  ASSERT_NE(msd_driver, nullptr);
  msd_driver.reset();
}
