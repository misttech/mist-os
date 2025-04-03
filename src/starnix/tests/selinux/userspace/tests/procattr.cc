// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

namespace {

TEST(ProcAttr, Current) {
  LoadPolicy("minimal_policy.pp");

  // Attempting to read the process' current context should return a value.
  EXPECT_THAT(ReadTaskAttr("current"), IsOk("system_u:unconfined_r:unconfined_t:s0"));
}

}  // namespace
