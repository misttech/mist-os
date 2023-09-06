// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/virtualization/tests/lib/guest_test.h"

namespace {

class ZirconSmokeTest : public GuestTest<ZirconEnclosedGuest> {};

TEST_F(ZirconSmokeTest, Boot) {
  std::string result;
  zx_status_t status = Execute({"echo", "Boot Complete!"}, &result);

  EXPECT_EQ(ZX_OK, status);
  EXPECT_EQ(result, "Boot Complete!\n");
}

}  // namespace
