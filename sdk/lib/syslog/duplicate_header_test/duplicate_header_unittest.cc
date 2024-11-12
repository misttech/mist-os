// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.diagnostics/cpp/fidl.h>
#include <lib/syslog/logger.h>
#include <lib/syslog/structured_backend/fuchsia_syslog.h>

#include <gtest/gtest.h>
#include <sdk/lib/syslog/cpp/log_level.h>

template <typename T, size_t Count>
static constexpr bool MultiEquals(const T values[Count]) {
  for (size_t i = 1; i < Count; i++) {
    if (values[i] != values[i - 1]) {
      return false;
    }
  }
  return true;
}

TEST(HeaderTest, CompileTimeAsserts) {
  // NOTE: Please ensure that all 4 headers above are updated
  // BEFORE approving a change to this file. All 4 files must be
  // manually kept in-sync, and this test needs to be kept up-to-date
  // to prevent future inadvertent breakages. Reviewers MUST make
  // sure that anything new added to those header files
  // are properly tested in this file prior to approval.
  constexpr int traces[] = {FX_LOG_TRACE, fuchsia_logging::LogSeverity::Trace, FUCHSIA_LOG_TRACE,
                            static_cast<uint8_t>(fuchsia_diagnostics::Severity::kTrace)};
  static_assert(MultiEquals<int, 4>(traces));
  constexpr int debugs[] = {FX_LOG_DEBUG, fuchsia_logging::LogSeverity::Debug, FUCHSIA_LOG_DEBUG,
                            static_cast<uint8_t>(fuchsia_diagnostics::Severity::kDebug)};
  static_assert(MultiEquals<int, 4>(debugs));
  constexpr int infos[] = {FX_LOG_INFO, fuchsia_logging::LogSeverity::Info, FUCHSIA_LOG_INFO,
                           static_cast<uint8_t>(fuchsia_diagnostics::Severity::kInfo)};
  static_assert(MultiEquals<int, 4>(infos));
  constexpr int errors[] = {FX_LOG_ERROR, fuchsia_logging::LogSeverity::Error, FUCHSIA_LOG_ERROR,
                            static_cast<uint8_t>(fuchsia_diagnostics::Severity::kError)};
  static_assert(MultiEquals<int, 4>(errors));
  constexpr int warns[] = {FX_LOG_WARNING, fuchsia_logging::LogSeverity::Warn, FUCHSIA_LOG_WARNING,
                           static_cast<uint8_t>(fuchsia_diagnostics::Severity::kWarn)};
  static_assert(MultiEquals<int, 4>(warns));
  constexpr int fatals[] = {FX_LOG_FATAL, fuchsia_logging::LogSeverity::Fatal, FUCHSIA_LOG_FATAL,
                            static_cast<uint8_t>(fuchsia_diagnostics::Severity::kFatal)};
  static_assert(MultiEquals<int, 4>(fatals));
  constexpr int nones[] = {FX_LOG_NONE, FUCHSIA_LOG_NONE};
  static_assert(MultiEquals<int, 2>(nones));
  constexpr int severity_steps[] = {FX_LOG_SEVERITY_STEP_SIZE, FUCHSIA_LOG_SEVERITY_STEP_SIZE};
  static_assert(MultiEquals<int, 2>(severity_steps));
}

int main(int argc, char **argv) {
  ::testing::InitGoogleTest(&argc, argv);
  return RUN_ALL_TESTS();
}
