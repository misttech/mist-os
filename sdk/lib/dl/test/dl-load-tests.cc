// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "dl-impl-tests.h"
#include "dl-system-tests.h"

// It's too much hassle the generate ELF test modules on a system where the
// host code is not usually built with ELF, so don't bother trying to test any
// of the ELF-loading logic on such hosts.  Unfortunately this means not
// discovering any <dlfcn.h> API differences from another non-ELF system that
// has that API, such as macOS.
#ifndef __ELF__
#error "This file should not be used on non-ELF hosts."
#endif

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
#ifdef __Fuchsia__
    dl::testing::DlImplTests<dl::testing::TestFuchsia>,
#endif
// TODO(https://fxbug.dev/324650368): Test fixtures currently retrieve files
// from different prefixed locations depending on the platform. Find a way
// to use a singular API to return the prefixed path specific to the platform so
// that the TestPosix fixture can run on Fuchsia as well.
#ifndef __Fuchsia__
    // libdl's POSIX test fixture can also be tested on Fuchsia and is included
    // for any ELF supported host.
    dl::testing::DlImplTests<dl::testing::TestPosix>,
#endif
    dl::testing::DlSystemTests>;

TYPED_TEST_SUITE(DlTests, TestTypes);

TYPED_TEST(DlTests, NotFound) {
  auto result = this->DlOpen("does_not_exist.so", RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_error());
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value().take_str(), "cannot open does_not_exist.so");
  } else {
    EXPECT_THAT(result.error_value().take_str(),
                MatchesRegex(".*does_not_exist.so:.*(No such file or directory|ZX_ERR_NOT_FOUND)"));
  }
}

TYPED_TEST(DlTests, InvalidMode) {
  if constexpr (!TestFixture::kCanValidateMode) {
    GTEST_SKIP() << "test requires dlopen to validate mode argment";
  }

  int bad_mode = -1;
  // The sanitizer runtimes (on non-Fuchsia hosts) intercept dlopen calls with
  // RTLD_DEEPBIND and make them fail without really calling -ldl's dlopen to
  // see if it would fail anyway.  So avoid having that flag set in the bad
  // mode argument.
#ifdef RTLD_DEEPBIND
  bad_mode &= ~RTLD_DEEPBIND;
#endif

  auto result = this->DlOpen("ret17.module.so", bad_mode);
  ASSERT_TRUE(result.is_error());
  EXPECT_EQ(result.error_value().take_str(), "invalid mode parameter")
      << "for mode argument " << bad_mode;
}

TYPED_TEST(DlTests, Basic) {
  auto result = this->DlOpen("ret17.module.so", RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());
  // Look up the "TestStart" function and call it, expecting it to return 17.
  auto sym_result = this->DlSym(result.value(), "TestStart");
  ASSERT_TRUE(sym_result.is_ok()) << result.error_value();
  ASSERT_TRUE(sym_result.value());
  int64_t (*func_ptr)();
  func_ptr = reinterpret_cast<int64_t (*)()>(reinterpret_cast<uintptr_t>(sym_result.value()));
  EXPECT_EQ(func_ptr(), 17);
}

// TODO(https://fxrev.dev/323419430): expect that libld-dep-a.so was needed.
TYPED_TEST(DlTests, BasicDep) {
  if constexpr (!TestFixture::kCanLookUpDeps) {
    GTEST_SKIP()
        << "TODO(https://fxbug.dev/324650368): test requires dlopen to locate dependencies.";
  }
  auto result = this->DlOpen("basic-dep.module.so", RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());
}

// TODO(https://fxrev.dev/323419430): expect that libld-dep-[a,b,c].so was needed.
TYPED_TEST(DlTests, IndirectDeps) {
  if constexpr (!TestFixture::kCanLookUpDeps) {
    GTEST_SKIP()
        << "TODO(https://fxbug.dev/324650368): test requires dlopen to locate dependencies.";
  }

  auto result = this->DlOpen("indirect-deps.module.so", RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());
}

TYPED_TEST(DlTests, MissingDependency) {
  // To clarify this condition, the test needs to be accurate in that its
  // searching the correct path for the dependency, but can't find it.
  if constexpr (!TestFixture::kCanLookUpDeps) {
    GTEST_SKIP()
        << "TODO(https://fxbug.dev/324650368): test requires dlopen to locate dependencies.";
  }

  auto result = this->DlOpen("missing-dep.module.so", RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_error());
  // Expect that the dependency lib to missing-dep.module.so cannot be found.
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value().take_str(), "cannot open libmissing-dep-dep.so");
  } else {
    EXPECT_THAT(
        result.error_value().take_str(),
        MatchesRegex(".*libmissing-dep-dep.so:.*(No such file or directory|ZX_ERR_NOT_FOUND)"));
  }
}

}  // namespace
