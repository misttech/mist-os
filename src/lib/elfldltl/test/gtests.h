// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_TEST_GTESTS_H_
#define SRC_LIB_ELFLDLTL_TEST_GTESTS_H_

#include <lib/elfldltl/diagnostics.h>

#include <filesystem>
#include <sstream>
#include <string>
#include <string_view>

#include <gtest/gtest.h>

constexpr auto ExpectOkDiagnostics() {
  auto fail = [](std::string_view error, auto&&... args) {
    std::stringstream os;
    elfldltl::OstreamDiagnostics(os).FormatError(error, args...);
    std::string message = os.str();
    if (message.back() == '\n')
      message.pop_back();
    ADD_FAILURE() << "Expected no diagnostics, got \"" << message << '"';
    return false;
  };
  return elfldltl::Diagnostics(fail, elfldltl::DiagnosticsFlags{.extra_checking = true});
}

std::filesystem::path GetTestDataPath(std::string_view filename);

#endif  // SRC_LIB_ELFLDLTL_TEST_GTESTS_H_
