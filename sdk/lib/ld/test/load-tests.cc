// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#ifdef __Fuchsia__
#include "ld-startup-create-process-tests.h"
#include "ld-startup-in-process-tests-zircon.h"
#include "ld-startup-spawn-process-tests-zircon.h"
#include "lib/ld/test/ld-remote-process-tests.h"
#else
#include "ld-startup-in-process-tests-posix.h"
#include "ld-startup-spawn-process-tests-posix.h"
#endif

namespace {

template <class Fixture>
using LdLoadTests = Fixture;

template <class Fixture>
using LdLoadFailureTests = Fixture;

// This lists all the types that are compatible with both LdLoadTests and LdLoadFailureTests.
template <class... Tests>
using TestTypes = ::testing::Types<
// TODO(fxbug.dev/130483): The separate-process tests require symbolic
// relocation so they can make the syscall to exit. The spawn-process
// tests also need a loader service to get ld.so.1 itself.
#ifdef __Fuchsia__
    ld::testing::LdStartupCreateProcessTests<>,
#else
    ld::testing::LdStartupSpawnProcessTests,
#endif
    Tests...>;

// This types are meaningul for the successful tests, LdLoadTests.
using LoadTypes = TestTypes<
// TODO(fxbug.dev/134320): LdRemoteProcessTests::Run doesn't actually run the
// test, instead it always returns 17. This isn't suitable for failure tests
// which don't return 17. When remote loading is implemented and these tests
// are actually run this can be moved into the default types in TestTypes.
#ifdef __Fuchsia__
    ld::testing::LdRemoteProcessTests,
#endif
    ld::testing::LdStartupInProcessTests>;

// These types are the types which are compatible with the failure tests, LdLoadFailureTests.
using FailTypes = TestTypes<>;

TYPED_TEST_SUITE(LdLoadTests, LoadTypes);
TYPED_TEST_SUITE(LdLoadFailureTests, FailTypes);

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

TYPED_TEST(LdLoadTests, Symbolic) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("symbolic-reloc"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, LoadWithNeeded) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  // There is only a reference to ld.so which doesn't need to be loaded to satisfy.
  ASSERT_NO_FATAL_FAILURE(this->Needed(std::initializer_list<std::string_view>{}));

  ASSERT_NO_FATAL_FAILURE(this->Load("ld-dep"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, BasicDep) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libld-dep-a.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("basic-dep"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, IndirectDeps) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({
      "libindirect-deps-a.so",
      "libindirect-deps-b.so",
      "libindirect-deps-c.so",
  }));

  ASSERT_NO_FATAL_FAILURE(this->Load("indirect-deps"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, PassiveAbiBasic) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("passive-abi-basic"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, SymbolicNamespace) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libld-dep-a.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("symbolic-namespace"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, ManyDeps) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({
      "libld-dep-a.so",
      "libld-dep-b.so",
      "libld-dep-f.so",
      "libld-dep-c.so",
      "libld-dep-d.so",
      "libld-dep-e.so",
  }));

  ASSERT_NO_FATAL_FAILURE(this->Load("many-deps"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, InitFini) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("init-fini"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, TlsExecOnly) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("tls-exec-only"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, TlsShlibOnly) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libtls-dep.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("tls-shlib-only"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, TlsExecShlib) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libtls-dep.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("tls-exec-shlib"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, TlsInitialExecAccess) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libtls-ie-dep.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("tls-ie"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, TlsGlobalDynamicAccess) {
  constexpr int64_t kReturnValue = 17;
  constexpr int64_t kSkipReturnValue = 77;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libtls-dep.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("tls-gd"));

  const int64_t return_value = this->Run();

  // Check the log before the return value so we've handled it in case we skip.
  this->ExpectLog("");

  if (return_value == kSkipReturnValue) {
    GTEST_SKIP() << "tls-gd module compiled with TLSDESC";
  }

  EXPECT_EQ(return_value, kReturnValue);
}

TYPED_TEST(LdLoadFailureTests, MissingSymbol) {
  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libld-dep-a.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("missing-sym"));

  EXPECT_EQ(this->Run(), this->kRunFailureForTrap);

  this->ExpectLog(R"(undefined symbol: b
startup dynamic linking failed with 1 errors and 0 warnings
)");
}

TYPED_TEST(LdLoadFailureTests, MissingDependency) {
  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({std::pair{"libmissing-dep-dep.so", false}}));

  ASSERT_NO_FATAL_FAILURE(this->Load("missing-dep"));

  EXPECT_EQ(this->Run(), this->kRunFailureForTrap);

  this->ExpectLog(R"(cannot open dependency: libmissing-dep-dep.so
startup dynamic linking failed with 1 errors and 0 warnings
)");
}

TYPED_TEST(LdLoadFailureTests, Relro) {
  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("relro"));

  EXPECT_EQ(this->Run(), this->kRunFailureForBadPointer);

  this->ExpectLog("");
}

}  // namespace
