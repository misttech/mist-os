// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/tests/mock_display_coordinator.h"

#include <fuchsia/hardware/display/cpp/fidl.h>

namespace scenic_impl {
namespace display {
namespace test {

DisplayCoordinatorObjects CreateMockDisplayCoordinator(fuchsia::hardware::display::Info info) {
  DisplayCoordinatorObjects coordinator_objs;

  zx::channel coordinator_channel_server;
  zx::channel coordinator_channel_client;
  FX_CHECK(ZX_OK ==
           zx::channel::create(0, &coordinator_channel_server, &coordinator_channel_client));

  coordinator_objs.mock = std::make_unique<MockDisplayCoordinator>(std::move(info));
  coordinator_objs.mock->Bind(std::move(coordinator_channel_server));

  coordinator_objs.interface_ptr =
      std::make_shared<fuchsia::hardware::display::CoordinatorSyncPtr>();
  coordinator_objs.interface_ptr->Bind(std::move(coordinator_channel_client));
  coordinator_objs.listener =
      std::make_unique<DisplayCoordinatorListener>(coordinator_objs.interface_ptr);

  return coordinator_objs;
}

void MockDisplayCoordinator::SetDisplayMode(fuchsia::hardware::display::types::DisplayId display_id,
                                            fuchsia::hardware::display::Mode mode) {
  FX_CHECK(std::find_if(display_info_.modes.begin(), display_info_.modes.end(),
                        [mode](const fuchsia::hardware::display::Mode& current_mode) {
                          return fidl::Equals(current_mode, mode);
                        }) != display_info_.modes.end());
}

void MockDisplayCoordinator::SendOnDisplayChangedEvent() {
  FX_CHECK(binding_.is_bound());
  events().OnDisplaysChanged(/*added=*/
                             {
                                 display_info_,
                             },
                             /*removed=*/{});
}

}  // namespace test
}  // namespace display
}  // namespace scenic_impl
