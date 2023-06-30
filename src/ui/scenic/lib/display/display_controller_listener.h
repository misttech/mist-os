// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_CONTROLLER_LISTENER_H_
#define SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_CONTROLLER_LISTENER_H_

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <lib/async/cpp/wait.h>
#include <lib/fit/function.h>
#include <lib/zx/channel.h>
#include <lib/zx/event.h>

#include "lib/fidl/cpp/synchronous_interface_ptr.h"

namespace scenic_impl {
namespace display {

// DisplayCoordinatorListener wraps a |fuchsia::hardware::display::Coordinator| interface, allowing
// registering for event callbacks.
class DisplayCoordinatorListener {
 public:
  using OnDisplaysChangedCallback =
      std::function<void(std::vector<fuchsia::hardware::display::Info> added,
                         std::vector<fuchsia::hardware::display::DisplayId> removed)>;
  using OnClientOwnershipChangeCallback = std::function<void(bool has_ownership)>;
  using OnVsyncCallback = std::function<void(
      fuchsia::hardware::display::DisplayId display_id, uint64_t timestamp,
      fuchsia::hardware::display::ConfigStamp applied_config_stamp, uint64_t cookie)>;

  // Binds to a Display fuchsia::hardware::display::Coordinator with channels |device| and
  // with display coordinator |coordinator|. |coordinator_handle| is the raw handle wrapped by
  // |coordinator|; unfortunately it must be passed separately since there's no  way to get it from
  // |coordinator|.
  //
  // If |device| or |coordinator_handle| is invalid, or |coordinator| is not bound, this instance is
  // invalid.
  DisplayCoordinatorListener(
      std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> coordinator);
  ~DisplayCoordinatorListener();

  // If any of the channels gets disconnected, |on_invalid| is invoked and this object becomes
  // invalid.
  void InitializeCallbacks(fit::closure on_invalid,
                           OnDisplaysChangedCallback on_displays_changed_cb,
                           OnClientOwnershipChangeCallback on_client_ownership_change_cb);

  // Removes all callbacks. Once this is done, there is no way to re-initialize the callbacks.
  void ClearCallbacks();

  void SetOnVsyncCallback(OnVsyncCallback vsync_callback);

  // Whether the connection to the display coordinator driver is still valid.
  bool valid() { return valid_; }

 private:
  void OnPeerClosedAsync(async_dispatcher_t* dispatcher, async::WaitBase* self, zx_status_t status,
                         const zx_packet_signal_t* signal);
  void OnEventMsgAsync(async_dispatcher_t* dispatcher, async::WaitBase* self, zx_status_t status,
                       const zx_packet_signal_t* signal);

  // The display coordinator driver binding.
  std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> coordinator_;

  // True if we're connected to |coordinator_|.
  bool valid_ = false;

  // |coordinator_| owns |coordinator_channel_handle_|, but save its handle here for use.
  zx_handle_t coordinator_channel_handle_ = 0;

  // Callback to invoke if we disconnect from |coordinator_|.
  fit::closure on_invalid_cb_ = nullptr;

  // True if InitializeCallbacks was called; it can only be called once.
  bool initialized_callbacks_ = false;

  // Waits for a ZX_CHANNEL_READABLE signal.
  async::WaitMethod<DisplayCoordinatorListener, &DisplayCoordinatorListener::OnEventMsgAsync>
      wait_event_msg_{this};
  async::WaitMethod<DisplayCoordinatorListener, &DisplayCoordinatorListener::OnPeerClosedAsync>
      wait_coordinator_closed_{this};

  // Used for dispatching events that we receive over the coordinator channel.
  // TODO(fxbug.dev/7520): Resolve this hack when synchronous interfaces support events.
  fidl::InterfacePtr<fuchsia::hardware::display::Coordinator> event_dispatcher_;
};

}  // namespace display
}  // namespace scenic_impl

#endif  // SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_CONTROLLER_LISTENER_H_
