// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/tests/mock_display_coordinator.h"

#include <fidl/fuchsia.hardware.display/cpp/hlcpp_conversion.h>
#include <fuchsia/hardware/display/cpp/fidl.h>
#include <lib/fidl/cpp/hlcpp_conversion.h>

#include "src/ui/scenic/lib/display/display_coordinator_listener.h"

namespace scenic_impl {
namespace display {
namespace test {

void MockDisplayCoordinator::SetDisplayMode(fuchsia::hardware::display::types::DisplayId display_id,
                                            fuchsia::hardware::display::Mode mode) {
  FX_CHECK(std::find_if(display_info_.modes.begin(), display_info_.modes.end(),
                        [mode](const fuchsia::hardware::display::Mode& current_mode) {
                          return fidl::Equals(current_mode, mode);
                        }) != display_info_.modes.end());
}

void MockDisplayCoordinator::SendOnDisplayChangedRequest() {
  FX_CHECK(binding_.is_bound());
  fuchsia_hardware_display::Info info =
      fidl::HLCPPToNatural(fuchsia::hardware::display::Info(display_info_));
  fit::result<fidl::OneWayStatus> result = listener()->OnDisplaysChanged({{.added =
                                                                               {
                                                                                   info,
                                                                               },
                                                                           .removed = {}}});
  FX_CHECK(result.is_ok());
}

}  // namespace test
}  // namespace display
}  // namespace scenic_impl
