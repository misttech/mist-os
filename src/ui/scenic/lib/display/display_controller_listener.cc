// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_controller_listener.h"

#include <lib/async/default.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/types.h>

namespace scenic_impl {
namespace display {

DisplayCoordinatorListener::DisplayCoordinatorListener(
    std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> coordinator)
    : coordinator_(std::move(coordinator)),
      coordinator_channel_handle_(coordinator_->unowned_channel()->get()) {
  valid_ = coordinator_channel_handle_ != 0 && coordinator_->is_bound();
  if (valid_) {
    // Listen for when the coordinator channel closes.
    wait_coordinator_closed_.set_object(coordinator_channel_handle_);
    wait_coordinator_closed_.set_trigger(ZX_CHANNEL_PEER_CLOSED);
    wait_coordinator_closed_.Begin(async_get_default_dispatcher());

    // Listen for events
    // TODO(https://fxbug.dev/42154967): Resolve this hack when synchronous interfaces support events.
    wait_event_msg_.set_object(coordinator_channel_handle_);
    wait_event_msg_.set_trigger(ZX_CHANNEL_READABLE);
    wait_event_msg_.Begin(async_get_default_dispatcher());
  }
}

DisplayCoordinatorListener::~DisplayCoordinatorListener() {
  ClearCallbacks();

  if (wait_event_msg_.object() != ZX_HANDLE_INVALID) {
    wait_event_msg_.Cancel();
  }
  if (wait_coordinator_closed_.object() != ZX_HANDLE_INVALID) {
    wait_coordinator_closed_.Cancel();
  }
}

void DisplayCoordinatorListener::InitializeCallbacks(
    fit::closure on_invalid, OnDisplaysChangedCallback on_displays_changed_cb,
    OnClientOwnershipChangeCallback on_client_ownership_change_cb) {
  FX_CHECK(!initialized_callbacks_);
  initialized_callbacks_ = true;

  on_invalid_cb_ = std::move(on_invalid);

  // TODO(https://fxbug.dev/42154967): Resolve this hack when synchronous interfaces support events.
  auto event_dispatcher =
      static_cast<fuchsia::hardware::display::Coordinator::Proxy_*>(event_dispatcher_.get());
  event_dispatcher->OnDisplaysChanged = std::move(on_displays_changed_cb);
  event_dispatcher->OnClientOwnershipChange = std::move(on_client_ownership_change_cb);
}

void DisplayCoordinatorListener::ClearCallbacks() {
  auto event_dispatcher =
      static_cast<fuchsia::hardware::display::Coordinator::Proxy_*>(event_dispatcher_.get());
  event_dispatcher->OnDisplaysChanged = nullptr;
  event_dispatcher->OnClientOwnershipChange = nullptr;
  event_dispatcher->OnVsync = nullptr;
  on_invalid_cb_ = nullptr;
}

void DisplayCoordinatorListener::SetOnVsyncCallback(OnVsyncCallback on_vsync_cb) {
  // TODO(https://fxbug.dev/42154967): Resolve this hack when synchronous interfaces support events.
  auto event_dispatcher =
      static_cast<fuchsia::hardware::display::Coordinator::Proxy_*>(event_dispatcher_.get());
  event_dispatcher->OnVsync = std::move(on_vsync_cb);
}

void DisplayCoordinatorListener::OnPeerClosedAsync(async_dispatcher_t* dispatcher,
                                                   async::WaitBase* self, zx_status_t status,
                                                   const zx_packet_signal_t* signal) {
  if (status != ZX_OK) {
    FX_LOGS(WARNING) << "scenic_impl::gfx::DisplayCoordinatorImpl: Error while waiting on "
                        "ZX_CHANNEL_PEER_CLOSED: "
                     << status;
    return;
  }

  if (signal->observed & ZX_CHANNEL_PEER_CLOSED) {
    valid_ = false;

    wait_coordinator_closed_.Cancel();
    if (on_invalid_cb_) {
      // We want |on_invalid_cb_| to be cleared when we're done, so move it out.
      auto callback = std::move(on_invalid_cb_);
      callback();
      // See warning below.
    }
    // Warning!
    // Don't do anything else after callback() is invoked, since |this| could be destroyed.
    return;
  }
  FX_NOTREACHED();
}

void DisplayCoordinatorListener::OnEventMsgAsync(async_dispatcher_t* dispatcher,
                                                 async::WaitBase* self, zx_status_t status,
                                                 const zx_packet_signal_t* signal) {
  // TODO(https://fxbug.dev/42154967): Resolve this hack when synchronous interfaces support events.
  if (status != ZX_OK) {
    FX_LOGS(WARNING)
        << "scenic_impl::gfx::DisplayCoordinatorImpl: Error while waiting on ZX_CHANNEL_READABLE: "
        << status;
    return;
  }
  if (signal->observed & ZX_CHANNEL_READABLE) {
    uint8_t byte_buffer[ZX_CHANNEL_MAX_MSG_BYTES];
    fidl::HLCPPIncomingMessage msg(fidl::BytePart(byte_buffer, ZX_CHANNEL_MAX_MSG_BYTES),
                                   fidl::HandleInfoPart());
    if (msg.Read(coordinator_channel_handle_, 0) != ZX_OK) {
      FX_LOGS(WARNING) << "Display coordinator callback read failed";
      return;
    }
    // Re-arm the wait.
    wait_event_msg_.Begin(async_get_default_dispatcher());

    static_cast<fuchsia::hardware::display::Coordinator::Proxy_*>(event_dispatcher_.get())
        ->Dispatch_(std::move(msg));
    return;
  }
  FX_NOTREACHED();
}

}  // namespace display
}  // namespace scenic_impl
