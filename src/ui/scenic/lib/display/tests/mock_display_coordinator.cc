// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/tests/mock_display_coordinator.h"

namespace scenic_impl {
namespace display {
namespace test {

DisplayCoordinatorObjects CreateMockDisplayCoordinator() {
  DisplayCoordinatorObjects coordinator_objs;

  zx::channel coordinator_channel_server;
  zx::channel coordinator_channel_client;
  FX_CHECK(ZX_OK ==
           zx::channel::create(0, &coordinator_channel_server, &coordinator_channel_client));

  coordinator_objs.mock = std::make_unique<MockDisplayCoordinator>();
  coordinator_objs.mock->Bind(std::move(coordinator_channel_server));

  coordinator_objs.interface_ptr =
      std::make_shared<fuchsia::hardware::display::CoordinatorSyncPtr>();
  coordinator_objs.interface_ptr->Bind(std::move(coordinator_channel_client));
  coordinator_objs.listener =
      std::make_unique<DisplayCoordinatorListener>(coordinator_objs.interface_ptr);

  return coordinator_objs;
}

}  // namespace test
}  // namespace display
}  // namespace scenic_impl
