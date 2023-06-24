// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_PROVIDER_LIB_CLIENT_HLCPP_H_
#define SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_PROVIDER_LIB_CLIENT_HLCPP_H_

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <lib/fpromise/promise.h>

namespace display {

struct CoordinatorHandlesHlcpp {
  fidl::InterfaceHandle<fuchsia::hardware::display::Coordinator> coordinator;
};

// Connects to the fuchsia.hardware.display.Provider service from the
// component's environment.
// Returns a promise which will be resolved when the display coordinator is
// obtained on success, if the display provider service is available and can
// be connected; otherwise returns a fpromise::error.
fpromise::promise<CoordinatorHandlesHlcpp, zx_status_t> GetCoordinatorHlcpp();

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_PROVIDER_LIB_CLIENT_HLCPP_H_
