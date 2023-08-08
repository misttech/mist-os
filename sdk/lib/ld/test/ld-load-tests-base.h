// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LD_LOAD_TESTS_BASE_H_
#define LIB_LD_TEST_LD_LOAD_TESTS_BASE_H_

#include <lib/elfldltl/testing/test-pipe-reader.h>

#include <memory>
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
  void InitLog(fbl::unique_fd& log_fd);

  void ExpectLog(std::string_view expected_log);

  ~LdLoadTestsBase();

 private:
  std::unique_ptr<elfldltl::testing::TestPipeReader> log_;
};

// The name given to elfldltl::GetTestLib to find the dynamic linker.
extern const std::string kLdStartupName;

inline std::string InProcessTestExecutable(std::string_view executable_name) {
  return std::string(executable_name) + ".in-process";
}

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_LOAD_TESTS_BASE_H_
