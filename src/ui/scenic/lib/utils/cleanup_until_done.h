// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_UTILS_CLEANUP_UNTIL_DONE_H_
#define SRC_UI_SCENIC_LIB_UTILS_CLEANUP_UNTIL_DONE_H_

#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/fit/function.h>
#include <lib/zx/time.h>

#include "src/lib/fxl/memory/weak_ptr.h"

namespace utils {

// Retries a `CleanupTask` lambda func until it returns true, meaning that cleanup has completed.
// This is useful in situations where cleanup cannot finish until some future condition is true.
// Retries are scheduled by posting a task on the default disptcher.
class CleanupUntilDone {
 public:
  using CleanupFunc = fit::function<bool()>;

  CleanupUntilDone(zx::duration delay, CleanupFunc func);

  CleanupUntilDone(const CleanupUntilDone&) = delete;
  CleanupUntilDone(CleanupUntilDone&&) = delete;
  CleanupUntilDone& operator=(const CleanupUntilDone&) = delete;
  CleanupUntilDone& operator=(CleanupUntilDone&&) = delete;

  // If a cleanup task has already been scheduled, return without doing anything.  Otherwise:
  //   - if `ok_to_run_immediately` is true, then cleanup will be attempted immediately
  //   - else, a task will be scheduled
  // The reason for `ok_to_run_immediately` is that sometimes you know that cleanup will be wasting
  // time if you call it immediately, so it's better to not try and instead schedule a task.
  void Cleanup(bool ok_to_run_immediately);

 private:
  zx::duration delay_;
  CleanupFunc func_;
  bool scheduled_ = false;
  fxl::WeakPtrFactory<CleanupUntilDone> weak_factory_;  // must be last
};

}  // namespace utils

#endif  // SRC_UI_SCENIC_LIB_UTILS_CLEANUP_UNTIL_DONE_H_
