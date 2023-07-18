// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/tests/utils/blocking_present.h"

namespace integration_tests {

void BlockingPresent(LoggingEventLoop* loop, fuchsia::ui::composition::FlatlandPtr& flatland,
                     cpp20::source_location caller) {
  // Request that the current frame be presented, and wait until Scenic indicates
  // that presentation is complete.
  bool presented = false;
  flatland.events().OnFramePresented = [&presented](auto) { presented = true; };
  flatland->Present({});
  loop->RunLoopUntil([&presented] { return presented; }, caller);
  flatland.events().OnFramePresented = nullptr;
}

}  // namespace integration_tests
