// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/tests/utils/logging_event_loop.h"

#include <lib/async-loop/testing/cpp/real_loop.h>
#include <lib/syslog/cpp/macros.h>

namespace integration_tests {

namespace internal {
/// Wraps `RealLoop`, publicly exposing the subset of APIs used by `LoggingEventLoop`.
///
/// This is necessary because:
/// 1. `LoggingEventLoop` wants to "override" methods provided by `RealLoop`,
/// 2. BUT `RealLoop` declares those methods as non-`virtual`,
/// 3. AND `RealLoop` declares those methods as `protected`.
///
/// Because of #2, having `LoggingEventLoop` inherit from `RealLoop` would be risky.
/// Specifically, code that has a reference to a `LoggingEventLoop` might upcast
/// that reference to `RealLoop`. That would cause the logging code in `LoggingEventLoop`
/// to be ignored.
///
/// Because of #3, `LoggingEventLoop` can't simply hold an instance of `RealLoop`, and
/// forward calls to that instance: the APIs needed by `LoggingEventLoop` are only visible
/// to subclasses of `RealLoop`.
class RealLoopWrapper : public loop_fixture::RealLoop {
 public:
  RealLoopWrapper() = default;
  ~RealLoopWrapper() = default;

  bool RunLoopWithTimeout(zx::duration timeout) { return RealLoop::RunLoopWithTimeout(timeout); }

  void RunLoopUntil(fit::function<bool()> condition) {
    RealLoop::RunLoopUntil(std::move(condition));
  }

  bool RunLoopWithTimeoutOrUntil(fit::function<bool()> condition, zx::duration timeout) {
    return RealLoop::RunLoopWithTimeoutOrUntil(std::move(condition), timeout);
  }

  void RunLoopUntilIdle() { RealLoop::RunLoopUntilIdle(); }
};
}  // namespace internal

namespace {
/// Returns a string describing the provided source location.
static std::string ToString(const cpp20::source_location& location) {
  std::string file = location.file_name() ? location.file_name() : "<unknown>";
  std::string line = std::to_string(location.line());
  std::string func = location.function_name() ? location.function_name() : "<unknown>";
  return file + ":" + line + " (" + func + ")";
}
}  // namespace

LoggingEventLoop::LoggingEventLoop() : loop_(std::make_unique<internal::RealLoopWrapper>()) {}

LoggingEventLoop::~LoggingEventLoop() {}

bool LoggingEventLoop::RunLoopWithTimeout(zx::duration timeout, cpp20::source_location caller) {
  FX_LOGS(INFO) << "Running until timeout (from " << ToString(caller) << ")";
  return loop_->RunLoopWithTimeout(timeout);
}

void LoggingEventLoop::RunLoopUntil(fit::function<bool()> condition,
                                    cpp20::source_location caller) {
  FX_LOGS(INFO) << "Waiting for condition from " << ToString(caller);
  loop_->RunLoopUntil(std::move(condition));
}

bool LoggingEventLoop::RunLoopWithTimeoutOrUntil(fit::function<bool()> condition,
                                                 zx::duration timeout,
                                                 cpp20::source_location caller) {
  FX_LOGS(INFO) << "Waiting for condition or timeout from " << ToString(caller);
  return loop_->RunLoopWithTimeoutOrUntil(std::move(condition), timeout);
}

void LoggingEventLoop::RunLoopUntilIdle(cpp20::source_location caller) {
  FX_LOGS(INFO) << "Running until idle (from " << ToString(caller) << ")";
  loop_->RunLoopUntilIdle();
}

}  // namespace integration_tests
