// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_E2E_TESTS_SCRIPT_TEST_H_
#define SRC_DEVELOPER_DEBUG_E2E_TESTS_SCRIPT_TEST_H_

#include <cstdint>
#include <filesystem>
#include <fstream>
#include <string>
#include <string_view>
#include <utility>

#include "src/developer/debug/e2e_tests/e2e_test.h"
#include "src/developer/debug/zxdb/console/mock_console.h"

namespace zxdb {

class ScriptTest : public E2eTest, public MockConsole::OutputObserver {
 public:
  explicit ScriptTest(std::string path) : script_path_(std::move(path)) {}

  void TestBody() override;

  // Implements |MockConsole::OutputObserver|.
  void OnOuput(const OutputBuffer& output) override;

  // Scan the directory and register all script tests.
  static void RegisterScriptTests();

  void OnTestExited(const std::string& url) override;

 private:
  // Process the next lines in the script until we meet a new output pattern.
  // This will either set |expected_output_pattern_| or finish the test.
  void ProcessUntilNextOutput();

  std::string script_path_;

  std::ifstream script_file_;

  // The pattern of a single line that |OnOutput| is expecting.
  std::string expected_output_pattern_;

  // Useful for debugging when timeout.
  std::string output_for_debug_;
  int line_number_ = 0;
};

}  // namespace zxdb

#endif  // SRC_DEVELOPER_DEBUG_E2E_TESTS_SCRIPT_TEST_H_
