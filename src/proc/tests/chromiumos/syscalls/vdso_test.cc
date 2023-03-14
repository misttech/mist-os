// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/auxv.h>

#include <gtest/gtest.h>

TEST(VdsoTest, AtSysinfoEhdrPresent) {
  uintptr_t addr = (uintptr_t)getauxval(AT_SYSINFO_EHDR);

  EXPECT_NE(addr, 0ul);
}
