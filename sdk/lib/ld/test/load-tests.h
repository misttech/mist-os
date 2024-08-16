// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LOAD_TESTS_H_
#define LIB_LD_TEST_LOAD_TESTS_H_

#include <gtest/gtest.h>

#ifdef __Fuchsia__
#include "ld-remote-process-tests.h"
#include "ld-startup-create-process-tests.h"
#include "ld-startup-in-process-tests-zircon.h"
#include "ld-startup-spawn-process-tests-zircon.h"
#else
#include "ld-startup-in-process-tests-posix.h"
#include "ld-startup-spawn-process-tests-posix.h"
#endif

namespace ld::testing {

template <class Fixture>
using LdLoadTests = Fixture;

template <class Fixture>
using LdLoadFailureTests = Fixture;

// This lists all the types that are compatible with both LdLoadTests and LdLoadFailureTests.
template <class... Tests>
using TestTypes = ::testing::Types<
// TODO(https://fxbug.dev/42080760): The separate-process tests require symbolic
// relocation so they can make the syscall to exit. The spawn-process
// tests also need a loader service to get ld.so.1 itself.
#ifdef __Fuchsia__
    ld::testing::LdStartupCreateProcessTests<>,  //
    ld::testing::LdRemoteProcessTests,
#else
    ld::testing::LdStartupSpawnProcessTests,
#endif
    Tests...>;

// These types are meaningul for the successful tests, LdLoadTests.
using LoadTypes = TestTypes<ld::testing::LdStartupInProcessTests>;

// These types are the types which are compatible with the failure tests,
// LdLoadFailureTests.
using FailTypes = TestTypes<>;

TYPED_TEST_SUITE(LdLoadTests, LoadTypes);
TYPED_TEST_SUITE(LdLoadFailureTests, FailTypes);

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LOAD_TESTS_H_
