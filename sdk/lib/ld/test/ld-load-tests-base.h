// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LD_LOAD_TESTS_BASE_H_
#define LIB_LD_TEST_LD_LOAD_TESTS_BASE_H_

#include <lib/elfldltl/diagnostics-ostream.h>
#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/elfldltl/testing/test-pipe-reader.h>

#include <memory>
#include <sstream>
#include <string>
#include <string_view>

#include <fbl/unique_fd.h>

namespace ld::testing {

// This is a common (additional) base class for test fixture classes.  It
// handles the log (stderr pipe) to the test.  The InitLog() function
// initializes the log and the destructor ensures that ExpectLog has been
// called (unless the test is bailing out anyway).
class LdLoadTestsBase {
 public:
  // Non-Fuchsia cases and in-process cases don't have an implicit dependency
  // on a vDSO from the module/test-start.cc code.
  static constexpr std::string_view kTestExecutableNeedsVdso{};

  // An indicator to GTEST of whether the test fixture supports the following
  // features so that it may skip related tests if not supported.
  static constexpr bool kCanCollectLog = true;

  void InitLog(fbl::unique_fd& log_fd);

  void ExpectLog(std::string_view expected_log);

  std::string CollectLog();

  // This is the least-common-denominator version used in non-Fuchsia tests.
  // All the Fuchsia-specific test fixtures should override this (e.g. via
  // LdLoadZirconLdsvcTestsBase).
  void Needed(std::initializer_list<std::string_view> names);

  void Needed(std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs);

  ~LdLoadTestsBase();

 protected:
  static constexpr std::string_view kTestExecutableInProcessSuffix = ".in-process";

  // This is overridden by LdStartupInProcessTests.
  static constexpr std::string_view kTestExecutableSuffix = {};

  template <class... Reports>
  void ExpectLogErrors(const elfldltl::testing::ExpectedErrorList<Reports...>& diag) {
    std::stringstream os;
    diag.report().ReportTo(elfldltl::OstreamDiagnosticsReport(os));
    os << "startup dynamic linking failed with " << sizeof...(Reports)
       << " errors and 0 warnings\n";
    ExpectLog(std::move(os).str());
  }

  template <class Derived, class... Reports>
  static void StartupLoadAndFail(Derived& self, std::string_view name,
                                 elfldltl::testing::ExpectedErrorList<Reports...> diag) {
    ASSERT_NO_FATAL_FAILURE(self.Load(name));
    EXPECT_EQ(self.Run(), Derived::kRunFailureForTrap);
    self.ExpectLogErrors(diag);
    std::move(diag).Release();
  }

 private:
  std::unique_ptr<elfldltl::testing::TestPipeReader> log_;
};

// The name given to elfldltl::GetTestLib to find the dynamic linker.
extern const std::string kLdStartupName;

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_LOAD_TESTS_BASE_H_
