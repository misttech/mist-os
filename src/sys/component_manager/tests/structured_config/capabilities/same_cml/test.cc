// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "src/sys/component_manager/tests/structured_config/capabilities/same_cml/config.h"

namespace {

// This test is checking that the CML configuration capability overrides the values set in the
// CVF.
TEST(ScTest, CheckValues) {
  config::Config c = config::Config::TakeFromStartupHandle();
  // CVF sets this to false, CML sets this to true.
  ASSERT_EQ(c.my_flag(), true);
}
}  // namespace
