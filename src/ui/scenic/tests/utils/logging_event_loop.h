// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_TESTS_UTILS_LOGGING_EVENT_LOOP_H_
#define SRC_UI_SCENIC_TESTS_UTILS_LOGGING_EVENT_LOOP_H_

#include <lib/fit/function.h>
#include <lib/stdcompat/source_location.h>
#include <lib/zx/time.h>

#include <memory>

namespace integration_tests {

namespace internal {
class RealLoopWrapper;
}

/// `LoggingEventLoop` provides a subset of the `RealLoop` API, and adds logging.
/// Feel free to expose more of the `RealLoop` API as needed.
class LoggingEventLoop {
 protected:
  LoggingEventLoop();
  ~LoggingEventLoop();

  bool RunLoopWithTimeout(zx::duration timeout,
                          cpp20::source_location caller = cpp20::source_location::current());

  void RunLoopUntil(fit::function<bool()> condition,
                    cpp20::source_location caller = cpp20::source_location::current());

  bool RunLoopWithTimeoutOrUntil(fit::function<bool()> condition, zx::duration timeout,
                                 cpp20::source_location caller = cpp20::source_location::current());

  void RunLoopUntilIdle(cpp20::source_location caller = cpp20::source_location::current());

 private:
  std::unique_ptr<internal::RealLoopWrapper> loop_;
};

}  // namespace integration_tests

#endif  // SRC_UI_SCENIC_TESTS_UTILS_LOGGING_EVENT_LOOP_H_
