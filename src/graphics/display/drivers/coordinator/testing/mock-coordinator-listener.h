// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_MOCK_COORDINATOR_LISTENER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_MOCK_COORDINATOR_LISTENER_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/vsync-ack-cookie.h"

namespace display {

class MockCoordinatorListener
    : public fidl::WireServer<fuchsia_hardware_display::CoordinatorListener> {
 public:
  using OnDisplaysChangedCallback =
      fit::function<void(std::vector<fuchsia_hardware_display::wire::Info> added_displays,
                         std::vector<DisplayId> removed_display_ids)>;
  using OnClientOwnershipChangeCallback = fit::function<void(bool has_ownership)>;
  using OnVsyncCallback =
      fit::function<void(DisplayId display_id, zx::time timestamp, ConfigStamp applied_config_stamp,
                         VsyncAckCookie cookie)>;

  MockCoordinatorListener(OnDisplaysChangedCallback on_displays_changed_callback,
                          OnVsyncCallback on_vsync_callback,
                          OnClientOwnershipChangeCallback on_client_ownership_change_callback);

  ~MockCoordinatorListener() override;

  MockCoordinatorListener(const MockCoordinatorListener&) = delete;
  MockCoordinatorListener(MockCoordinatorListener&&) = delete;
  MockCoordinatorListener& operator=(const MockCoordinatorListener&) = delete;
  MockCoordinatorListener& operator=(MockCoordinatorListener&&) = delete;

  // Must be only called once for each `MockCoordinatorListener` instance.
  // `server_end` must be valid.
  void Bind(fidl::ServerEnd<fuchsia_hardware_display::CoordinatorListener> server_end,
            async_dispatcher_t& dispatcher);

  // [`fuchsia.hardware.display/CoordinatorListener`]:
  void OnDisplaysChanged(OnDisplaysChangedRequestView request,
                         OnDisplaysChangedCompleter::Sync& completer) override;
  void OnVsync(OnVsyncRequestView request, OnVsyncCompleter::Sync& completer) override;
  void OnClientOwnershipChange(OnClientOwnershipChangeRequestView request,
                               OnClientOwnershipChangeCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_display::CoordinatorListener> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  std::optional<fidl::ServerBindingRef<fuchsia_hardware_display::CoordinatorListener>>& binding() {
    return binding_;
  }

 private:
  const OnDisplaysChangedCallback on_displays_changed_callback_;
  const OnVsyncCallback on_vsync_callback_;
  const OnClientOwnershipChangeCallback on_client_ownership_change_callback_;

  async_dispatcher_t* binding_dispatcher_ = nullptr;
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_display::CoordinatorListener>> binding_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_MOCK_COORDINATOR_LISTENER_H_
