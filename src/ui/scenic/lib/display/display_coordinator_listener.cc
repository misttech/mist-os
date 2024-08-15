// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_coordinator_listener.h"

#include <fidl/fuchsia.hardware.display/cpp/hlcpp_conversion.h>
#include <lib/async/default.h>
#include <lib/fidl/cpp/hlcpp_conversion.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/types.h>

namespace scenic_impl {
namespace display {

DisplayCoordinatorListener::DisplayCoordinatorListener(
    fidl::ServerEnd<fuchsia_hardware_display::CoordinatorListener> coordinator_listener_server,
    OnDisplaysChangedCallback on_displays_changed, OnVsyncCallback on_vsync,
    OnClientOwnershipChangeCallback on_client_ownership_change)
    : on_displays_changed_(std::move(on_displays_changed)),
      on_vsync_(std::move(on_vsync)),
      on_client_ownership_change_(std::move(on_client_ownership_change)),
      binding_(fidl::BindServer(async_get_default_dispatcher(),
                                std::move(coordinator_listener_server), this)) {}

DisplayCoordinatorListener::~DisplayCoordinatorListener() {}

void DisplayCoordinatorListener::OnDisplaysChanged(OnDisplaysChangedRequest& request,
                                                   OnDisplaysChangedCompleter::Sync& completer) {
  if (on_displays_changed_) {
    std::vector<fuchsia::hardware::display::Info> added = fidl::NaturalToHLCPP(request.added());
    std::vector<fuchsia::hardware::display::types::DisplayId> removed =
        fidl::NaturalToHLCPP(request.removed());
    on_displays_changed_(std::move(added), std::move(removed));
  }
}

void DisplayCoordinatorListener::OnVsync(OnVsyncRequest& request,
                                         OnVsyncCompleter::Sync& completer) {
  if (on_vsync_) {
    fuchsia::hardware::display::types::DisplayId display_id =
        fidl::NaturalToHLCPP(request.display_id());
    uint64_t timestamp = static_cast<uint64_t>(request.timestamp());
    fuchsia::hardware::display::types::ConfigStamp config_stamp =
        fidl::NaturalToHLCPP(request.applied_config_stamp());
    uint64_t cookie = request.cookie().value();
    on_vsync_(display_id, timestamp, config_stamp, cookie);
  }
}

void DisplayCoordinatorListener::OnClientOwnershipChange(
    OnClientOwnershipChangeRequest& request, OnClientOwnershipChangeCompleter::Sync& completer) {
  if (on_client_ownership_change_) {
    on_client_ownership_change_(request.has_ownership());
  }
}

void DisplayCoordinatorListener::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_display::CoordinatorListener> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FX_LOGS(WARNING) << "Unknown event received: # " << metadata.method_ordinal;
}

}  // namespace display
}  // namespace scenic_impl
