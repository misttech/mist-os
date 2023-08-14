// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#ifdef __Fuchsia__
#include "ld-startup-create-process-tests.h"
#include "ld-startup-in-process-tests-zircon.h"
#else
#include "ld-startup-in-process-tests-posix.h"
#include "ld-startup-spawn-process-tests-posix.h"
#endif

namespace {

template <class Fixture>
using LdLoadTests = Fixture;

using LoadTypes = ::testing::Types<
// TODO(fxbug.dev/130483): The separate-process tests require symbolic
// relocation so they can make the syscall to exit.
#if 0  // def __Fuchsia__
    ld::testing::LdStartupCreateProcessTests<>,
#endif
#ifndef __Fuchsia__  // TODO(fxbug.dev/130483): add fdio_spawn version
    ld::testing::LdStartupSpawnProcessTests,
#endif
    ld::testing::LdStartupInProcessTests>;

TYPED_TEST_SUITE(LdLoadTests, LoadTypes);

TYPED_TEST(LdLoadTests, Basic) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("ret17"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, Relative) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("relative-reloc"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

}  // namespace
