// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_PROVIDER_LIB_CLIENT_HLCPP_H_
#define SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_PROVIDER_LIB_CLIENT_HLCPP_H_

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <lib/fpromise/promise.h>

namespace display {

class DevFsCoordinatorFactoryHlcpp;

struct CoordinatorHandlesHlcpp {
  fidl::InterfaceHandle<fuchsia::hardware::display::Coordinator> coordinator;
};

// Connect to the fuchsia::hardware::display::Provider service, and return a promise which will be
// resolved when the display coordinator is obtained. One variant uses the explicitly-provided
// service, and the other variant finds the service in the component's environment.
//
// If the display coordinator cannot be obtained for some reason, |devfs_provider_opener| will be
// used to bind a connection if given. Otherwise, the handles will be null.
//
// |devfs_provider_opener| binding to Display is done internally and does not need any published
// services. This breaks the dependency in Scenic service startup.
fpromise::promise<CoordinatorHandlesHlcpp> GetCoordinatorHlcpp(
    std::shared_ptr<fuchsia::hardware::display::ProviderPtr> provider);

fpromise::promise<CoordinatorHandlesHlcpp> GetCoordinatorHlcpp(
    DevFsCoordinatorFactoryHlcpp* devfs_provider_opener = nullptr);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_PROVIDER_LIB_CLIENT_HLCPP_H_
