// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <algorithm>
#include <fstream>
#include <optional>
#include <string>
#include <vector>

#include <re2/re2.h>
#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/diagnostic_types.h"
#include "tools/fidl/fidlc/include/fidl/diagnostics.h"
#include "tools/fidl/fidlc/tests/error_test.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

// Reads a file into a string.
std::optional<std::string> ReadFile(const std::string& path) {
  std::ifstream input(path);
  if (!input) {
    return std::nullopt;
  }
  std::stringstream buf;
  buf << input.rdbuf();
  return buf.str();
}

// Returns all the good/fi-NNNN*.test.fidl paths found in _fi-NNNN.md.
std::optional<std::vector<std::string>> GoodFidlPaths(const fidl::DiagnosticDef& def) {
  auto id = def.FormatId();
  auto path = TestLibrary::TestFilePath("error-catalog/_" + id + ".md");
  auto content = ReadFile(path);
  if (!content) {
    return std::nullopt;
  }
  re2::StringPiece text = content.value();
  RE2 pattern =
      R"RE(gerrit_path="tools/fidl/fidlc/tests/fidl/(good/fi-(\d+)(?:-[a-z])?\.test.fidl)")RE";
  std::vector<std::string> result;
  std::string group1;
  fidl::ErrorId group2;
  while (RE2::FindAndConsume(&text, pattern, &group1, &group2)) {
    if (group2 != def.id) {
      ADD_FAILURE("%s references %s which is for the wrong error ID", path.c_str(), group1.c_str());
    }
    result.push_back(group1);
  }
  if (result.empty()) {
    ADD_FAILURE("%s does not reference any good .test.fidl files", path.c_str());
  }
  return result;
}

TEST(ErrcatDocsTests, IndexIsComplete) {
  auto path = TestLibrary::TestFilePath("errcat.md");
  std::ifstream input(path);
  ASSERT_TRUE(input);
  auto it = std::begin(fidl::kAllDiagnosticDefs);
  const auto end = std::end(fidl::kAllDiagnosticDefs);
  std::string line;
  std::string prefix = "<<error-catalog/";
  while (it != end && std::getline(input, line)) {
    if (line.substr(0, prefix.size()) == prefix) {
      auto id = (*it)->FormatId();
      ASSERT_STREQ(line, "<<error-catalog/_" + id + ".md>>",
                   "unexpected entry in %s; either %s was not next in sequence, or it is marked "
                   "documented=false in diagnostics.h and should not appear",
                   path.c_str(), id.c_str());
      it = std::find_if(std::next(it), end,
                        [](const fidl::DiagnosticDef* def) { return def->opts.documented; });
    }
  }
  if (it != end) {
    ADD_FAILURE("%s did not contain all diagnostics; missing %s to %s", path.c_str(),
                (*it)->FormatId().c_str(), (*std::prev(end))->FormatId().c_str());
  }
}

TEST(ErrcatDocsTests, RedirectsAreComplete) {
  auto path = TestLibrary::TestFilePath("_redirects.yaml");
  auto redirects = ReadFile(path).value();
  for (auto def : fidl::kAllDiagnosticDefs) {
    auto id = def->FormatId();
    auto entry = "- from: /fuchsia-src/error/" + id +
                 "\n  to: /fuchsia-src/reference/fidl/language/errcat.md#" + id;
    if (def->opts.documented) {
      EXPECT_SUBSTR(redirects, entry, "%s is missing a redirect for %s", path.c_str(), id.c_str());
    } else {
      EXPECT_NOT_SUBSTR(
          redirects, entry,
          "%s unexpectedly has a redirect for %s, which is marked documented=false in diagnostics.h",
          path.c_str(), id.c_str());
    }
  }
}

TEST(ErrcatDocsTests, MarkdownFilesExist) {
  for (auto def : fidl::kAllDiagnosticDefs) {
    auto id = def->FormatId();
    auto path = TestLibrary::TestFilePath("error-catalog/_" + id + ".md");
    bool exists = std::filesystem::exists(path);
    if (def->opts.documented && !exists) {
      ADD_FAILURE(
          "%s is marked documented=true in diagnostics.h, but the Markdown file %s DOES NOT exist",
          id.c_str(), path.c_str());
    } else if (!def->opts.documented && exists) {
      ADD_FAILURE(
          "%s is marked documented=false in diagnostics.h, but the Markdown file %s DOES exists",
          id.c_str(), path.c_str());
    }
  }
}

TEST(ErrcatDocsTests, DocsAreAccurate) {
  for (auto def : fidl::kAllDiagnosticDefs) {
    if (!def->opts.documented) {
      continue;
    }
    auto id = def->FormatId();
    auto path = TestLibrary::TestFilePath("error-catalog/_" + id + ".md");
    auto content = ReadFile(path);
    if (!content) {
      // Will be caught by ErrcatDocsTests.MarkdownFilesExist.
      continue;
    }
    if (def->kind == fidl::DiagnosticKind::kRetired) {
      auto prefix = "## " + id + " {:#" + id + " .hide-from-toc}";
      EXPECT_STREQ(content.value().substr(0, prefix.size()), prefix,
                   "first line of %s is incorrect", path.c_str());
      EXPECT_SUBSTR(
          content.value(), "Deprecated: This error code has been retired.",
          "%s is DiagnosticKind::kRetired, but the Markdown file does not say it is retired: %s",
          id.c_str(), path.c_str());
    } else {
      auto prefix = "## " + id + ":";
      EXPECT_STREQ(content.value().substr(0, prefix.size()), prefix,
                   "first line of %s is incorrect", path.c_str());
      EXPECT_SUBSTR(content.value(), "{:#" + id + "}",
                    "missing the expected heading id attribute in %s", path.c_str());
    }
  }
}

TEST(ErrcatDocsTests, GoodFilesAreTested) {
  auto path = TestLibrary::TestFilePath("errcat_good_tests.cc");
  auto source_file = ReadFile(path).value();
  for (auto def : fidl::kAllDiagnosticDefs) {
    if (!def->opts.documented || def->kind == fidl::DiagnosticKind::kRetired) {
      continue;
    }
    if (def->id == fidl::ErrGeneratedZeroValueOrdinal.id) {
      // This error has no examples because it is impossible to test.
      continue;
    }
    auto fidl_paths = GoodFidlPaths(*def);
    if (!fidl_paths.has_value()) {
      // Will be caught by ErrcatDocsTests.MarkdownFilesExist.
      continue;
    }
    for (auto& fidl : fidl_paths.value()) {
      auto quoted = "\"" + fidl + "\"";
      EXPECT_TRUE(source_file.find(quoted) != std::string::npos,
                  "%s does not contain a test for %s", path.c_str(), quoted.c_str());
    }
  }
}

TEST(ErrcatDocsTests, RetiredErrorsAreNotTested) {
  auto path = TestLibrary::TestFilePath("errcat_good_tests.cc");
  auto source_file = ReadFile(path).value();
  for (auto def : fidl::kAllDiagnosticDefs) {
    if (def->kind != fidl::DiagnosticKind::kRetired) {
      continue;
    }
    auto index = source_file.find("\"good/" + def->FormatId());
    if (index == std::string::npos) {
      continue;
    }
    auto line_number =
        1 + static_cast<int>(std::count(source_file.data(), source_file.data() + index, '\n'));
    ADD_FAILURE("%s:%d unexpectedly contains test for %s, which is retired", path.c_str(),
                line_number, def->FormatId().c_str());
  }
}

}  // namespace
