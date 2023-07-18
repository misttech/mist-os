// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_TESTS_UTILS_BLOCKING_PRESENT_H_
#define SRC_UI_SCENIC_TESTS_UTILS_BLOCKING_PRESENT_H_

#include <fuchsia/ui/composition/cpp/fidl.h>
#include <lib/stdcompat/source_location.h>

#include "src/ui/scenic/tests/utils/logging_event_loop.h"

namespace integration_tests {

// Invokes `flatland->Present()` and then uses `loop` to loop until Scenic indicates that
// the frame has been presented.
void BlockingPresent(LoggingEventLoop* loop, fuchsia::ui::composition::FlatlandPtr& flatland,
                     cpp20::source_location = cpp20::source_location::current());

}  // namespace integration_tests

#endif  // SRC_UI_SCENIC_TESTS_UTILS_BLOCKING_PRESENT_H_
