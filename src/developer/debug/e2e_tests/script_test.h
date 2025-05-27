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
  void OnOutput(const OutputBuffer& output) override;

  // Scan the directory and register all script tests.
  static void RegisterScriptTests();

  void OnTestExited(const std::string& url) override;

 private:
  // Process the script until the next command or line of output. Returns false when the next
  // command has been reached, which means all expected output should be matched by corresponding
  // output events. Returns true when there are more expected output lines to match.
  bool ProcessScriptLines();

  // Dispatches the given |command| after the output of the currently executing has been completely
  // processed. This is always issued asynchronously, but may be more than one tick of the message
  // loop, depending on the amount of output being matched against for a given command.
  void DispatchNextCommandWhenReady(const std::string& command);

  std::string script_path_;

  std::ifstream script_file_;

  // The pattern of a single line that |OnOutput| is expecting.
  std::string expected_output_pattern_;

  // Indicates that we're processing the output of a command and we should not dispatch further
  // commands until this has been reset to false.
  bool processing_ = false;

  // This is passed to a FuzzyMatcher object to communicate that it should not expect the order of
  // strings to be exact. This is the case for various kinds of commands, like `async-backtrace` and
  // `locals`, but should NOT be used for things like `frame`.
  bool allow_out_of_order_output_ = false;

  // Useful for debugging when timeout.
  std::string output_for_debug_;
  int line_number_ = 0;
};

}  // namespace zxdb

#endif  // SRC_DEVELOPER_DEBUG_E2E_TESTS_SCRIPT_TEST_H_
