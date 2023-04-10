// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/utils/cleanup_until_done.h"

namespace utils {

CleanupUntilDone::CleanupUntilDone(zx::duration delay, CleanupFunc func)
    : delay_(delay), func_(std::move(func)), weak_factory_(this) {}

void CleanupUntilDone::Cleanup(bool ok_to_run_immediately) {
  if (scheduled_) {
    // Cleanup has already been scheduled.
    return;
  } else if (ok_to_run_immediately && func_()) {
    // Cleanup func succeeded
  } else {
    // Still more cleanup to do.
    scheduled_ = true;
    async::PostDelayedTask(
        async_get_default_dispatcher(),
        [weak = weak_factory_.GetWeakPtr()] {
          if (weak) {
            // Recursively reschedule if cleanup is incomplete.
            weak->scheduled_ = false;
            weak->Cleanup(/*ok_to_run_immediately*/ true);
          }
        },
        delay_);
  }
}

}  // namespace utils
