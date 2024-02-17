// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#ifdef __ELF__
#include "dl-impl-tests.h"
#endif
#include "dl-system-tests.h"

namespace {

using ::testing::MatchesRegex;

template <class Fixture>
using DlTests = Fixture;

// This lists the test fixture classes to run DlTests tests against. The
// DlImplTests fixture is a framework for testing the implementation in
// libdl and the DlSystemTests fixture proxies to the system-provided dynamic
// linker. These tests ensure that both dynamic linker implementations meet
// expectations and behave the same way, with exceptions noted within the test.
using TestTypes = ::testing::Types<
#ifdef __ELF__
    dl::testing::DlImplTests,
#endif
    dl::testing::DlSystemTests>;

TYPED_TEST_SUITE(DlTests, TestTypes);

TYPED_TEST(DlTests, NotFound) {
  auto result = this->DlOpen("does_not_exist.so", RTLD_NOW | RTLD_LOCAL);
  EXPECT_TRUE(result.is_error());
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value(), "cannot open dependency: test/lib/does_not_exist.so");
  } else {
    EXPECT_THAT(result.error_value(),
                MatchesRegex(".*does_not_exist.so:.*(No such file or directory|ZX_ERR_NOT_FOUND)"));
  }
}
}  // namespace
