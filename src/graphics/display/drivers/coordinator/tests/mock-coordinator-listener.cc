// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/tests/mock-coordinator-listener.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <zircon/assert.h>

#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/vsync-ack-cookie.h"

namespace display {

MockCoordinatorListener::MockCoordinatorListener(
    OnDisplaysChangedCallback on_displays_changed_callback, OnVsyncCallback on_vsync_callback,
    OnClientOwnershipChangeCallback on_client_ownership_change_callback)
    : on_displays_changed_callback_(std::move(on_displays_changed_callback)),
      on_vsync_callback_(std::move(on_vsync_callback)),
      on_client_ownership_change_callback_(std::move(on_client_ownership_change_callback)) {}

MockCoordinatorListener::~MockCoordinatorListener() = default;

void MockCoordinatorListener::Bind(
    fidl::ServerEnd<fuchsia_hardware_display::CoordinatorListener> server_end,
    async_dispatcher_t& dispatcher) {
  ZX_DEBUG_ASSERT(server_end.is_valid());
  ZX_DEBUG_ASSERT(!binding_.has_value());
  binding_ = fidl::BindServer(&dispatcher, std::move(server_end), this);
}

void MockCoordinatorListener::OnDisplaysChanged(OnDisplaysChangedRequestView request,
                                                OnDisplaysChangedCompleter::Sync& completer) {
  std::vector added_display_infos(request->added.begin(), request->added.end());
  std::vector<DisplayId> removed_display_ids;
  for (fuchsia_hardware_display_types::wire::DisplayId fidl_id : request->removed) {
    removed_display_ids.push_back(ToDisplayId(fidl_id));
  }

  on_displays_changed_callback_(std::move(added_display_infos), std::move(removed_display_ids));
}

void MockCoordinatorListener::OnVsync(OnVsyncRequestView request,
                                      OnVsyncCompleter::Sync& completer) {
  on_vsync_callback_(ToDisplayId(request->display_id), zx::time(request->timestamp),
                     ToConfigStamp(request->applied_config_stamp),
                     ToVsyncAckCookie(request->cookie));
}

void MockCoordinatorListener::OnClientOwnershipChange(
    OnClientOwnershipChangeRequestView request, OnClientOwnershipChangeCompleter::Sync& completer) {
  if (on_client_ownership_change_callback_) {
    on_client_ownership_change_callback_(request->has_ownership);
  }
}

void MockCoordinatorListener::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_display::CoordinatorListener> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

}  // namespace display
