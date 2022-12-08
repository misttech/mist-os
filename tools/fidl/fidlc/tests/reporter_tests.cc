// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/diagnostic_types.h"
#include "tools/fidl/fidlc/include/fidl/experimental_flags.h"
#include "tools/fidl/fidlc/include/fidl/fixables.h"
#include "tools/fidl/fidlc/include/fidl/reporter.h"
#include "tools/fidl/fidlc/include/fidl/source_span.h"
#include "tools/fidl/fidlc/include/fidl/virtual_source_file.h"

namespace {

using fidl::Diagnostic;
using fidl::ErrorDef;
using fidl::FixableErrorDef;
using fidl::FixableWarningDef;
using fidl::Reporter;
using fidl::SourceSpan;
using fidl::VirtualSourceFile;
using fidl::WarningDef;

// TODO(fxbug.dev/108248): Remove once all outstanding errors are documented.
using fidl::UndocumentedErrorDef;
const fidl::ErrorId kTestUndocumentedErrorId = 9995;
const std::string kTestUndocumentedErrorIdStr = "fi-9995";

const fidl::ErrorId kTestErrorId = 9998;
const std::string kTestErrorIdStr = "fi-9998";
const fidl::ErrorId kTestWarningId = 9999;
const std::string kTestWarningIdStr = "fi-9999";
const fidl::ErrorId kTestFixableErrorId = 9997;
const std::string kTestFixableErrorIdStr = "fi-9997";
const fidl::ErrorId kTestFixableWarningId = 9996;
const std::string kTestFixableWarningIdStr = "fi-9996";

const std::string kFakeBinaryLocation = "fake/bin/path";
const std::string kFixMeTagOpen = "[[[ FIXME ]]]";
const std::string kFixMeTagClose = "[[[ /FIXME ]]]";

// TODO(fxbug.dev/108248): Remove once all outstanding errors are documented.
constexpr UndocumentedErrorDef<kTestUndocumentedErrorId, std::string_view, std::string_view>
    UndocumentedErrTest("This undocumented test error has one string param '{}' and another '{}'.");

constexpr ErrorDef<kTestErrorId, std::string_view, std::string_view> ErrTest(
    "This test error has one string param '{}' and another '{}'.");
constexpr WarningDef<kTestWarningId, std::string_view, std::string_view> WarnTest(
    "This test warning has one string param '{}' and another '{}'.");

constexpr FixableErrorDef<kTestFixableErrorId, fidl::Fixable::Kind::kNoop, std::string_view,
                          std::string_view>
    FixableErrTest("This test error has one string param '{}' and another '{}'.");
constexpr FixableWarningDef<kTestFixableWarningId, fidl::Fixable::Kind::kNoop, std::string_view,
                            std::string_view>
    FixableWarnTest("This test warning has one string param '{}' and another '{}'.");

fidl::SourceManager FakeSourceManager(std::string prefix, uint8_t files_per_lib) {
  // Always use a positive single-digit number.
  ZX_ASSERT(files_per_lib > 0);
  ZX_ASSERT(files_per_lib < 10);

  fidl::SourceManager out;
  for (uint8_t i = 0; i < files_per_lib; i++) {
    char filename[12];
    std::snprintf(filename, sizeof filename, "file_%d.fidl", i);
    out.AddSourceFile(std::make_unique<VirtualSourceFile>(prefix + filename));
  }
  return out;
}

std::vector<fidl::SourceManager> FakeSources(uint8_t deps, uint8_t files_per_lib) {
  // Always use a single-digit number.
  ZX_ASSERT(deps < 10);

  std::vector<fidl::SourceManager> out;
  for (uint8_t i = 0; i < deps; i++) {
    char prefix[7];
    std::snprintf(prefix, sizeof prefix, "dep_%d_", i);
    out.push_back(FakeSourceManager(prefix, files_per_lib));
  }
  out.push_back(FakeSourceManager("lib_", files_per_lib));
  return out;
}

// TODO(fxbug.dev/108248): Remove once all outstanding errors are documented.
TEST(ReporterTests, ReportUndocumentedErrorFormatParams) {
  Reporter reporter;
  VirtualSourceFile file("fake");
  SourceSpan span("span text", file);
  reporter.Fail(UndocumentedErrTest, span, "param1", "param2");

  const auto& errors = reporter.errors();
  ASSERT_EQ(errors.size(), 1);
  ASSERT_EQ(errors[0]->span, span);
  EXPECT_EQ(errors[0]->PrintId(), kTestUndocumentedErrorIdStr);
  EXPECT_NOT_SUBSTR(errors[0]->Print(reporter.program_invocation()).c_str(),
                    kTestUndocumentedErrorIdStr);
  EXPECT_NOT_SUBSTR(errors[0]->msg.c_str(), kTestUndocumentedErrorIdStr);
  EXPECT_SUBSTR(errors[0]->msg.c_str(),
                "This undocumented test error has one string param 'param1' and another 'param2'.");
}

// TODO(fxbug.dev/108248): Remove once all outstanding errors are documented.
TEST(ReporterTests, MakeUndocumentedErrorThenReportIt) {
  Reporter reporter;
  VirtualSourceFile file("fake");
  SourceSpan span("span text", file);
  std::unique_ptr<Diagnostic> diag =
      Diagnostic::MakeError(UndocumentedErrTest, span, "param1", "param2");
  reporter.Report(std::move(diag));

  const auto& errors = reporter.errors();
  ASSERT_EQ(errors.size(), 1);
  ASSERT_EQ(errors[0]->span, span);
  EXPECT_EQ(errors[0]->PrintId(), kTestUndocumentedErrorIdStr);
  EXPECT_NOT_SUBSTR(errors[0]->Print(reporter.program_invocation()).c_str(),
                    kTestUndocumentedErrorIdStr);
  EXPECT_NOT_SUBSTR(errors[0]->msg.c_str(), kTestUndocumentedErrorIdStr);
  ASSERT_SUBSTR(errors[0]->msg.c_str(),
                "This undocumented test error has one string param 'param1' and another 'param2'.");
}

TEST(ReporterTests, ReportErrorFormatParams) {
  Reporter reporter;
  VirtualSourceFile file("fake");
  SourceSpan span("span text", file);
  reporter.Fail(ErrTest, span, "param1", "param2");

  const auto& errors = reporter.errors();
  ASSERT_EQ(errors.size(), 1);
  ASSERT_EQ(errors[0]->span, span);
  EXPECT_EQ(errors[0]->PrintId(), kTestErrorIdStr);
  EXPECT_SUBSTR(errors[0]->Print(reporter.program_invocation()).c_str(), kTestErrorIdStr);
  EXPECT_NOT_SUBSTR(errors[0]->msg.c_str(), kTestErrorIdStr);
  EXPECT_SUBSTR(errors[0]->msg.c_str(),
                "This test error has one string param 'param1' and another 'param2'.");
}

TEST(ReporterTests, MakeErrorThenReportIt) {
  Reporter reporter;
  VirtualSourceFile file("fake");
  SourceSpan span("span text", file);
  std::unique_ptr<Diagnostic> diag = Diagnostic::MakeError(ErrTest, span, "param1", "param2");
  reporter.Report(std::move(diag));

  const auto& errors = reporter.errors();
  ASSERT_EQ(errors.size(), 1);
  ASSERT_EQ(errors[0]->span, span);
  EXPECT_EQ(errors[0]->PrintId(), kTestErrorIdStr);
  EXPECT_SUBSTR(errors[0]->Print(reporter.program_invocation()).c_str(), kTestErrorIdStr);
  EXPECT_NOT_SUBSTR(errors[0]->msg.c_str(), kTestErrorIdStr);
  ASSERT_SUBSTR(errors[0]->msg.c_str(),
                "This test error has one string param 'param1' and another 'param2'.");
}

TEST(ReporterTests, ReportWarningFormatParams) {
  Reporter reporter;
  VirtualSourceFile file("fake");
  SourceSpan span("span text", file);
  reporter.Warn(WarnTest, span, "param1", "param2");

  const auto& warnings = reporter.warnings();
  ASSERT_EQ(warnings.size(), 1);
  ASSERT_EQ(warnings[0]->span, span);
  EXPECT_EQ(warnings[0]->PrintId(), kTestWarningIdStr);
  EXPECT_SUBSTR(warnings[0]->Print(reporter.program_invocation()).c_str(), kTestWarningIdStr);
  EXPECT_NOT_SUBSTR(warnings[0]->msg.c_str(), kTestWarningIdStr);
  EXPECT_SUBSTR(warnings[0]->msg.c_str(),
                "This test warning has one string param 'param1' and another 'param2'.");
}

TEST(ReporterTests, MakeWarningThenReportIt) {
  Reporter reporter;
  VirtualSourceFile file("fake");
  SourceSpan span("span text", file);
  std::unique_ptr<Diagnostic> diag = Diagnostic::MakeWarning(WarnTest, span, "param1", "param2");
  reporter.Report(std::move(diag));

  const auto& warnings = reporter.warnings();
  ASSERT_EQ(warnings.size(), 1);
  ASSERT_EQ(warnings[0]->span, span);
  EXPECT_EQ(warnings[0]->PrintId(), kTestWarningIdStr);
  EXPECT_SUBSTR(warnings[0]->Print(reporter.program_invocation()).c_str(), kTestWarningIdStr);
  EXPECT_NOT_SUBSTR(warnings[0]->msg.c_str(), kTestWarningIdStr);
  EXPECT_SUBSTR(warnings[0]->msg.c_str(),
                "This test warning has one string param 'param1' and another 'param2'.");
}

TEST(ReporterTests, ReportFixableErrorFormatParams) {
  std::vector<fidl::SourceManager> sources = FakeSources(1, 2);
  fidl::ExperimentalFlags experimental_flags = fidl::ExperimentalFlags();
  experimental_flags.EnableFlag(fidl::ExperimentalFlags::Flag::kNoop);
  Reporter reporter(kFakeBinaryLocation, experimental_flags, &sources);
  SourceSpan span("fixable span text", *sources.back().sources().back().get());
  reporter.FixableFail(FixableErrTest, span, "param1", "param2");

  const auto& errors = reporter.errors();
  EXPECT_TRUE(reporter.warnings().empty());
  ASSERT_EQ(errors.size(), 1);
  ASSERT_EQ(errors[0]->span, span);
  EXPECT_EQ(errors[0]->PrintId(), kTestFixableErrorIdStr);
  EXPECT_SUBSTR(errors[0]->Print(reporter.program_invocation()).c_str(), kTestFixableErrorIdStr);
  EXPECT_NOT_SUBSTR(errors[0]->msg.c_str(), kTestFixableErrorIdStr);
  EXPECT_SUBSTR(errors[0]->msg.c_str(),
                "This test error has one string param 'param1' and another 'param2'.");

  EXPECT_SUBSTR(errors[0]->Print(reporter.program_invocation()).c_str(), kFixMeTagOpen);
  EXPECT_SUBSTR(errors[0]->Print(reporter.program_invocation()).c_str(),
                ">>> " + kFakeBinaryLocation + "/fidl-fix --fix=" +
                    std::string(fidl::Fixable::Get(fidl::Fixable::Kind::kNoop).name) +
                    " --experiment=noop --dep=dep_0_file_0.fidl,dep_0_file_1.fidl" +
                    " lib_file_0.fidl lib_file_1.fidl");
  EXPECT_SUBSTR(errors[0]->Print(reporter.program_invocation()).c_str(), kFixMeTagClose);
}

TEST(ReporterTests, MakeFixableErrorThenReportIt) {
  std::vector<fidl::SourceManager> sources = FakeSources(0, 3);
  fidl::ExperimentalFlags experimental_flags = fidl::ExperimentalFlags();
  experimental_flags.EnableFlag(fidl::ExperimentalFlags::Flag::kNoop);
  Reporter reporter(kFakeBinaryLocation, experimental_flags, &sources);
  SourceSpan span("fixable span text", *sources.back().sources().back().get());
  std::unique_ptr<Diagnostic> diag =
      Diagnostic::MakeError(FixableErrTest, span, "param1", "param2");
  reporter.Report(std::move(diag));

  const auto& errors = reporter.errors();
  EXPECT_TRUE(reporter.warnings().empty());
  ASSERT_EQ(errors.size(), 1);
  ASSERT_EQ(errors[0]->span, span);
  EXPECT_EQ(errors[0]->PrintId(), kTestFixableErrorIdStr);
  EXPECT_SUBSTR(errors[0]->Print(reporter.program_invocation()).c_str(), kTestFixableErrorIdStr);
  EXPECT_NOT_SUBSTR(errors[0]->msg.c_str(), kTestFixableErrorIdStr);
  ASSERT_SUBSTR(errors[0]->msg.c_str(),
                "This test error has one string param 'param1' and another 'param2'.");

  EXPECT_SUBSTR(errors[0]->Print(reporter.program_invocation()).c_str(), kFixMeTagOpen);
  EXPECT_SUBSTR(errors[0]->Print(reporter.program_invocation()).c_str(),
                ">>> " + kFakeBinaryLocation + "/fidl-fix --fix=" +
                    std::string(fidl::Fixable::Get(fidl::Fixable::Kind::kNoop).name) +
                    " --experiment=noop lib_file_0.fidl lib_file_1.fidl lib_file_2.fidl");
  EXPECT_SUBSTR(errors[0]->Print(reporter.program_invocation()).c_str(), kFixMeTagClose);
}

TEST(ReporterTests, ReportFixableWarningFormatParams) {
  std::vector<fidl::SourceManager> sources = FakeSources(1, 2);
  fidl::ExperimentalFlags experimental_flags = fidl::ExperimentalFlags();
  experimental_flags.EnableFlag(fidl::ExperimentalFlags::Flag::kNoop);
  Reporter reporter(kFakeBinaryLocation, experimental_flags, &sources);
  SourceSpan span("fixable span text", *sources.back().sources().back().get());
  reporter.FixableWarn(FixableWarnTest, span, "param1", "param2");

  const auto& warnings = reporter.warnings();
  EXPECT_TRUE(reporter.errors().empty());
  ASSERT_EQ(warnings.size(), 1);
  ASSERT_EQ(warnings[0]->span, span);
  EXPECT_EQ(warnings[0]->PrintId(), kTestFixableWarningIdStr);
  EXPECT_SUBSTR(warnings[0]->Print(reporter.program_invocation()).c_str(),
                kTestFixableWarningIdStr);
  EXPECT_NOT_SUBSTR(warnings[0]->msg.c_str(), kTestFixableWarningIdStr);
  EXPECT_SUBSTR(warnings[0]->msg.c_str(),
                "This test warning has one string param 'param1' and another 'param2'.");

  EXPECT_SUBSTR(warnings[0]->Print(reporter.program_invocation()).c_str(), kFixMeTagOpen);
  EXPECT_SUBSTR(warnings[0]->Print(reporter.program_invocation()).c_str(),
                ">>> " + kFakeBinaryLocation + "/fidl-fix --fix=" +
                    std::string(fidl::Fixable::Get(fidl::Fixable::Kind::kNoop).name) +
                    " --experiment=noop --dep=dep_0_file_0.fidl,dep_0_file_1.fidl" +
                    " lib_file_0.fidl lib_file_1.fidl");
  EXPECT_SUBSTR(warnings[0]->Print(reporter.program_invocation()).c_str(), kFixMeTagClose);
}

TEST(ReporterTests, MakeFixableWarningThenReportIt) {
  std::vector<fidl::SourceManager> sources = FakeSources(0, 3);
  fidl::ExperimentalFlags experimental_flags = fidl::ExperimentalFlags();
  experimental_flags.EnableFlag(fidl::ExperimentalFlags::Flag::kNoop);
  Reporter reporter(kFakeBinaryLocation, experimental_flags, &sources);
  SourceSpan span("fixable span text", *sources.back().sources().back().get());
  std::unique_ptr<Diagnostic> diag =
      Diagnostic::MakeWarning(FixableWarnTest, span, "param1", "param2");
  reporter.Report(std::move(diag));

  const auto& warnings = reporter.warnings();
  EXPECT_TRUE(reporter.errors().empty());
  ASSERT_EQ(warnings.size(), 1);
  ASSERT_EQ(warnings[0]->span, span);
  EXPECT_EQ(warnings[0]->PrintId(), kTestFixableWarningIdStr);
  EXPECT_SUBSTR(warnings[0]->Print(reporter.program_invocation()).c_str(),
                kTestFixableWarningIdStr);
  EXPECT_NOT_SUBSTR(warnings[0]->msg.c_str(), kTestFixableWarningIdStr);
  EXPECT_SUBSTR(warnings[0]->msg.c_str(),
                "This test warning has one string param 'param1' and another 'param2'.");

  EXPECT_SUBSTR(warnings[0]->Print(reporter.program_invocation()).c_str(), kFixMeTagOpen);
  EXPECT_SUBSTR(warnings[0]->Print(reporter.program_invocation()).c_str(),
                ">>> " + kFakeBinaryLocation + "/fidl-fix --fix=" +
                    std::string(fidl::Fixable::Get(fidl::Fixable::Kind::kNoop).name) +
                    " --experiment=noop lib_file_0.fidl lib_file_1.fidl lib_file_2.fidl");
  EXPECT_SUBSTR(warnings[0]->Print(reporter.program_invocation()).c_str(), kFixMeTagClose);
}

TEST(ReporterTests, CheckpointNumNewErrors) {
  Reporter reporter;
  VirtualSourceFile file("fake");
  SourceSpan span("span text", file);
  reporter.Fail(ErrTest, span, "1", "");

  auto checkpoint = reporter.Checkpoint();
  EXPECT_EQ(checkpoint.NumNewErrors(), 0);
  EXPECT_TRUE(checkpoint.NoNewErrors());

  reporter.Fail(ErrTest, span, "2", "");
  EXPECT_EQ(checkpoint.NumNewErrors(), 1);
  EXPECT_FALSE(checkpoint.NoNewErrors());

  reporter.Fail(ErrTest, span, "3", "");
  EXPECT_EQ(checkpoint.NumNewErrors(), 2);
  EXPECT_FALSE(checkpoint.NoNewErrors());
}

}  // namespace
