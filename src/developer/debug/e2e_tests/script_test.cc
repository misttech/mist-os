// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/e2e_tests/script_test.h"

#include <cstdint>
#include <cstdlib>
#include <fstream>
#include <string>
#include <string_view>

#include <gtest/gtest.h>

#include "src/developer/debug/e2e_tests/fuzzy_matcher.h"
#include "src/developer/debug/shared/string_util.h"
#include "src/developer/debug/zxdb/common/host_util.h"
#include "src/lib/fxl/strings/trim.h"

namespace zxdb {

namespace {

constexpr uint64_t kDefaultTimeout = 3;  // in seconds
constexpr std::string_view kBuildType = ZXDB_E2E_TESTS_BUILD_TYPE;

}  // namespace

void ScriptTest::TestBody() {
  script_file_ = std::ifstream(script_path_);
  ASSERT_TRUE(script_file_) << "Fail to open " << script_path_;

  // Process directives first.
  uint64_t timeout = kDefaultTimeout;
  std::string line;
  while (std::getline(script_file_, line)) {
    line_number_++;
    if (line.empty())
      continue;
    if (debug::StringStartsWith(line, "##")) {
      std::string directive = std::string(fxl::TrimString(line.substr(2), " "));
      if (debug::StringStartsWith(directive, "require ")) {
        std::string requirement = directive.substr(8);
        if (kBuildType.find(requirement) == std::string::npos) {
          GTEST_SKIP() << "Skipped because of unmet requirement " << requirement;
        }
      } else if (debug::StringStartsWith(directive, "set timeout ")) {
        timeout = std::stoul(directive.substr(12));
      } else {
        GTEST_FAIL() << "Unknown directive: " << directive;
      }
    } else if (debug::StringStartsWith(line, "#")) {
      continue;
    } else {
      // Put the line back.
      script_file_.seekg(-static_cast<int>(line.size() + 1), std::ios::cur);
      line_number_--;
      break;
    }
  }

  // Adjust timeout when running on bots so we're less likely to flake.
  if (std::getenv("BUILDBUCKET_ID")) {
    timeout *= 5;
  }

  std::string error_msg;
  loop().PostTimer(FROM_HERE, timeout * 1000, [&]() {
    error_msg = "Failed to find pattern \"" + expected_output_pattern_ +
                "\" in the output:\n"
                "============================= BEGIN OUTPUT =============================\n" +
                output_for_debug_ +
                "============================== END OUTPUT ==============================";
    loop().QuitNow();
  });

  console().output_observers().AddObserver(this);
  ProcessUntilNextOutput();
  loop().Run();
  console().output_observers().RemoveObserver(this);

  if (!error_msg.empty()) {
    ADD_FAILURE_AT(script_path_.c_str(), line_number_) << error_msg;
  }
}

void ScriptTest::OnOutput(const OutputBuffer& output) {
  output_for_debug_ += output.AsString();
  if (!output_for_debug_.empty() && output_for_debug_.back() != '\n') {
    output_for_debug_ += '\n';
  }
  FuzzyMatcher matcher(output.AsString());
  while (matcher.MatchesLine(expected_output_pattern_)) {
    ProcessUntilNextOutput();
  }
}

void ScriptTest::ProcessUntilNextOutput() {
  std::string line;
  while (std::getline(script_file_, line)) {
    line_number_++;

    if (line.empty() || debug::StringStartsWith(line, "#")) {
      continue;
    }

    // Inputs
    if (debug::StringStartsWith(line, "[zxdb]")) {
      std::string command = std::string(fxl::TrimString(line.substr(6), " "));
      // Defer ProcessInputLine because it will trigger OnOutput synchronously.
      loop().PostTask(FROM_HERE,
                      [this, command]() { console().ProcessInputLine(std::string(command)); });
      output_for_debug_.clear();
      continue;
    }

    // Expected outputs
    expected_output_pattern_ = line;
    return;
  }

  // Done
  loop().QuitNow();
}

void ScriptTest::OnTestExited(const std::string& url) {
  // Insert a definitive marker for a test component being completed. Scripts that use `run-test`
  // will want to depend on this output so we remain listening for test_runner messages until it has
  // completely shutdown.
  loop().PostTask(FROM_HERE, [this, url]() { console().Output("Test Done: " + url, false); });
}

void ScriptTest::RegisterScriptTests() {
  std::filesystem::path test_scripts_dir =
      (std::filesystem::path(GetSelfPath()).parent_path() / ZXDB_E2E_TESTS_SCRIPTS_DIR)
          .lexically_normal();

  for (const auto& entry : std::filesystem::directory_iterator(test_scripts_dir)) {
    if (entry.path().extension() == ".script") {
      ::testing::RegisterTest("ScriptTest", entry.path().stem().c_str(), nullptr, nullptr,
                              entry.path().c_str(), 0,
                              [=]() { return new ScriptTest(entry.path()); });
    }
  }
}

}  // namespace zxdb
