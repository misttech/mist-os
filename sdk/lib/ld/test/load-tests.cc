// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#ifdef __Fuchsia__
#include "ld-startup-in-process-tests-zircon.h"
#else
#include "ld-startup-in-process-tests-posix.h"
#endif

namespace {

template <class Fixture>
using LdLoadTests = Fixture;

using LoadTypes = ::testing::Types<ld::testing::LdStartupInProcessTests>;

TYPED_TEST_SUITE(LdLoadTests, LoadTypes);

TYPED_TEST(LdLoadTests, Basic) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("ret17"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

}  // namespace
