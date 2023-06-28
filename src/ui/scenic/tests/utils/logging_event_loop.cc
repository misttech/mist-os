// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/tests/utils/logging_event_loop.h"

#include <lib/syslog/cpp/macros.h>

namespace integration_tests {

namespace {
/// Returns a string describing the provided source location.
static std::string ToString(const cpp20::source_location& location) {
  std::string file = location.file_name() ? location.file_name() : "<unknown>";
  std::string line = std::to_string(location.line());
  std::string func = location.function_name() ? location.function_name() : "<unknown>";
  return file + ":" + line + " (" + func + ")";
}
}  // namespace

using loop_fixture::RealLoop;

LoggingEventLoop::LoggingEventLoop() {}

LoggingEventLoop::~LoggingEventLoop() {}

bool LoggingEventLoop::RunLoopWithTimeout(zx::duration timeout, cpp20::source_location caller) {
  FX_LOGS(INFO) << "Running until timeout (from " << ToString(caller) << ")";
  return RealLoop::RunLoopWithTimeout(timeout);
}

void LoggingEventLoop::RunLoopUntil(fit::function<bool()> condition,
                                    cpp20::source_location caller) {
  FX_LOGS(INFO) << "Waiting for condition from " << ToString(caller);
  RealLoop::RunLoopUntil(std::move(condition));
}

bool LoggingEventLoop::RunLoopWithTimeoutOrUntil(fit::function<bool()> condition,
                                                 zx::duration timeout,
                                                 cpp20::source_location caller) {
  FX_LOGS(INFO) << "Waiting for condition or timeout from " << ToString(caller);
  return RealLoop::RunLoopWithTimeoutOrUntil(std::move(condition), timeout);
}

void LoggingEventLoop::RunLoopUntilIdle(cpp20::source_location caller) {
  FX_LOGS(INFO) << "Running until idle (from " << ToString(caller) << ")";
  RealLoop::RunLoopUntilIdle();
}

}  // namespace integration_tests
