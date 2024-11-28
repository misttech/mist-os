// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/testing/mock-coordinator-listener.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/assert.h>

#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/vsync-ack-cookie.h"

namespace display_coordinator {

MockCoordinatorListener::MockCoordinatorListener(
    OnDisplaysChangedCallback on_displays_changed_callback, OnVsyncCallback on_vsync_callback,
    OnClientOwnershipChangeCallback on_client_ownership_change_callback)
    : on_displays_changed_callback_(std::move(on_displays_changed_callback)),
      on_vsync_callback_(std::move(on_vsync_callback)),
      on_client_ownership_change_callback_(std::move(on_client_ownership_change_callback)) {}

MockCoordinatorListener::~MockCoordinatorListener() {
  if (binding_.has_value()) {
    ZX_ASSERT(binding_dispatcher_);
    // We can call Unbind() on any thread, but it's async and previously-started dispatches can
    // still be in-flight after this call.
    binding_->Unbind();
    // The Unbind() above will prevent starting any new dispatches, but previously-started
    // dispatches can still be in-flight. For this reason we must fence the Bind's dispatcher thread
    // before we delete stuff used during dispatch such as on_vsync_callback_.
    libsync::Completion done;
    zx_status_t post_status = async::PostTask(binding_dispatcher_, [&done] { done.Signal(); });
    ZX_ASSERT(post_status == ZX_OK);
    done.Wait();
    // Now it's safe to delete on_vsync_callback_ (for example).
  }
}

void MockCoordinatorListener::Bind(
    fidl::ServerEnd<fuchsia_hardware_display::CoordinatorListener> server_end,
    async_dispatcher_t& dispatcher) {
  ZX_DEBUG_ASSERT(server_end.is_valid());
  ZX_DEBUG_ASSERT(!binding_.has_value());
  binding_dispatcher_ = &dispatcher;
  binding_ = fidl::BindServer(&dispatcher, std::move(server_end), this);
}

void MockCoordinatorListener::OnDisplaysChanged(OnDisplaysChangedRequestView request,
                                                OnDisplaysChangedCompleter::Sync& completer) {
  std::vector added_display_infos(request->added.begin(), request->added.end());
  std::vector<display::DisplayId> removed_display_ids;
  for (fuchsia_hardware_display_types::wire::DisplayId fidl_id : request->removed) {
    removed_display_ids.push_back(display::ToDisplayId(fidl_id));
  }

  on_displays_changed_callback_(std::move(added_display_infos), std::move(removed_display_ids));
}

void MockCoordinatorListener::OnVsync(OnVsyncRequestView request,
                                      OnVsyncCompleter::Sync& completer) {
  on_vsync_callback_(display::ToDisplayId(request->display_id), zx::time(request->timestamp),
                     display::ToConfigStamp(request->applied_config_stamp),
                     display::ToVsyncAckCookie(request->cookie));
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

}  // namespace display_coordinator
