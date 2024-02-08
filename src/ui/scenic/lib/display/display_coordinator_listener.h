// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_COORDINATOR_LISTENER_H_
#define SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_COORDINATOR_LISTENER_H_

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/hardware/display/types/cpp/fidl.h>
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
                         std::vector<fuchsia::hardware::display::types::DisplayId> removed)>;
  using OnClientOwnershipChangeCallback = std::function<void(bool has_ownership)>;
  using OnVsyncCallback = std::function<void(
      fuchsia::hardware::display::types::DisplayId display_id, uint64_t timestamp,
      fuchsia::hardware::display::types::ConfigStamp applied_config_stamp, uint64_t cookie)>;

  // Creates a DisplayCoordinatorListener binding to a
  // [`fuchsia.hardware.display/Coordinator`] client.
  //
  // `coordinator` must be valid and bound to a channel.
  explicit DisplayCoordinatorListener(
      std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> coordinator);
  ~DisplayCoordinatorListener();

  // If any of the channels gets disconnected, |on_invalid| is invoked and this object becomes
  // invalid.
  void InitializeCallbacks(OnDisplaysChangedCallback on_displays_changed_cb,
                           OnClientOwnershipChangeCallback on_client_ownership_change_cb);

  // Removes all callbacks. Once this is done, there is no way to re-initialize the callbacks.
  void ClearCallbacks();

  void SetOnVsyncCallback(OnVsyncCallback vsync_callback);

 private:
  void OnEventMsgAsync(async_dispatcher_t* dispatcher, async::WaitBase* self, zx_status_t status,
                       const zx_packet_signal_t* signal);

  // The display coordinator driver binding.
  std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> coordinator_;

  // |coordinator_| owns |coordinator_channel_handle_|, but save its handle here for use.
  zx_handle_t coordinator_channel_handle_ = 0;

  // True if InitializeCallbacks was called; it can only be called once.
  bool initialized_callbacks_ = false;

  // Waits for a ZX_CHANNEL_READABLE signal.
  async::WaitMethod<DisplayCoordinatorListener, &DisplayCoordinatorListener::OnEventMsgAsync>
      wait_event_msg_{this};

  // Used for dispatching events that we receive over the coordinator channel.
  // TODO(https://fxbug.dev/42154967): Resolve this hack when synchronous interfaces support events.
  fidl::InterfacePtr<fuchsia::hardware::display::Coordinator> event_dispatcher_;
};

}  // namespace display
}  // namespace scenic_impl

#endif  // SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_COORDINATOR_LISTENER_H_
