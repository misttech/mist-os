// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/tests/utils/blocking_present.h"

#include <lib/syslog/cpp/macros.h>

namespace integration_tests {

void BlockingPresent(LoggingEventLoop* loop, fuchsia::ui::composition::FlatlandPtr& flatland,
                     cpp20::source_location caller) {
  // Initialize callbacks and callback state.
  bool presented = false;
  bool began = false;
  flatland.events().OnFramePresented = [&presented](auto) { presented = true; };
  flatland.events().OnNextFrameBegin = [&began](auto) { began = true; };

  // Request that the current frame be presented, and wait until Scenic indicates
  // that presentation is complete.
  flatland->Present({});
  FX_LOGS(INFO) << "Waiting for OnFramePresented";
  loop->RunLoopUntil([&presented] { return presented; }, caller);

  // Wait for `OnNextFrameBegin`. This ensures that `flatland` has present
  // credits available, and hence, the next `Present()` (if any) will not fail
  // due to `NO_PRESENTS_REMAINING`.
  FX_LOGS(INFO) << "Waiting for OnNextFrameBegin";
  loop->RunLoopUntil([&began] { return began; }, caller);

  // Reset callbacks.
  flatland.events().OnFramePresented = nullptr;
  flatland.events().OnNextFrameBegin = nullptr;
}

}  // namespace integration_tests
