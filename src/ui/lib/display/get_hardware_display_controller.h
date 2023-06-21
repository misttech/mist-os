// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/127211): Remove this file after migrating all clients to the
// headers in //src/graphics/display/testing.

#ifndef SRC_UI_LIB_DISPLAY_GET_HARDWARE_DISPLAY_CONTROLLER_H_
#define SRC_UI_LIB_DISPLAY_GET_HARDWARE_DISPLAY_CONTROLLER_H_

#include "src/graphics/display/testing/coordinator-provider-lib/client-hlcpp.h"

namespace ui_display {

using DisplayCoordinatorHandles = display::CoordinatorHandlesHlcpp;

inline fpromise::promise<DisplayCoordinatorHandles> GetHardwareDisplayCoordinator() {
  return display::GetCoordinatorHlcpp();
}

}  // namespace ui_display

#endif  // SRC_UI_LIB_DISPLAY_GET_HARDWARE_DISPLAY_CONTROLLER_H_
