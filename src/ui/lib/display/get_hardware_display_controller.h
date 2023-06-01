// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/127211): Remove this file after migrating all clients to the
// headers in //src/graphics/display/testing.

#ifndef SRC_UI_LIB_DISPLAY_GET_HARDWARE_DISPLAY_CONTROLLER_H_
#define SRC_UI_LIB_DISPLAY_GET_HARDWARE_DISPLAY_CONTROLLER_H_

#include "src/graphics/display/testing/coordinator-provider-lib/client-hlcpp.h"
#include "src/graphics/display/testing/coordinator-provider-lib/devfs-factory-hlcpp.h"

namespace ui_display {

using DisplayCoordinatorHandles = display::CoordinatorHandlesHlcpp;

using HardwareDisplayCoordinatorProviderImpl = display::DevFsCoordinatorFactoryHlcpp;

inline fpromise::promise<DisplayCoordinatorHandles> GetHardwareDisplayCoordinator(
    std::shared_ptr<fuchsia::hardware::display::ProviderPtr> provider) {
  return display::GetCoordinatorHlcpp(std::move(provider));
}

inline fpromise::promise<DisplayCoordinatorHandles> GetHardwareDisplayCoordinator(
    HardwareDisplayCoordinatorProviderImpl* hdcp_service_impl = nullptr) {
  return display::GetCoordinatorHlcpp(hdcp_service_impl);
}

}  // namespace ui_display

#endif  // SRC_UI_LIB_DISPLAY_GET_HARDWARE_DISPLAY_CONTROLLER_H_
