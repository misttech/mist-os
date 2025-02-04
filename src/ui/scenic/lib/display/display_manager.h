// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_MANAGER_H_
#define SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_MANAGER_H_

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <lib/fit/function.h>

#include <optional>

#include "src/lib/fxl/macros.h"
#include "src/ui/scenic/lib/display/display.h"
#include "src/ui/scenic/lib/display/display_coordinator_listener.h"
#include "src/ui/scenic/lib/utils/range_inclusive.h"

namespace scenic_impl {
namespace display {

struct DisplayModeConstraints {
  utils::RangeInclusive<int> width_px_range;
  utils::RangeInclusive<int> height_px_range;
  utils::RangeInclusive<int> refresh_rate_millihertz_range;

  bool ModeSatisfiesConstraints(const fuchsia_hardware_display_types::Mode& mode) const;
};

// Discovers and owns the default display coordinator, and waits for and exposes the default
// display.
class DisplayManager {
 public:
  // |display_available_cb| is a one-shot callback that is triggered when the first display is
  // observed, and cleared immediately afterward.
  explicit DisplayManager(fit::closure display_available_cb);
  DisplayManager(std::optional<fuchsia_hardware_display_types::DisplayId> i_can_haz_display_id,
                 std::optional<size_t> display_mode_index_override,
                 DisplayModeConstraints display_mode_constraints,
                 fit::closure display_available_cb);
  ~DisplayManager() = default;

  void BindDefaultDisplayCoordinator(
      fidl::ClientEnd<fuchsia_hardware_display::Coordinator> coordinator,
      fidl::ServerEnd<fuchsia_hardware_display::CoordinatorListener> coordinator_listener);

  // Gets information about the default display.
  // May return null if there isn't one.
  Display* default_display() const { return default_display_.get(); }

  // Only use this during Scenic initialization to pass a reference to FrameScheduler.
  std::shared_ptr<Display> default_display_shared() const { return default_display_; }

  std::shared_ptr<fidl::SyncClient<fuchsia_hardware_display::Coordinator>>
  default_display_coordinator() {
    return default_display_coordinator_;
  }

  std::shared_ptr<display::DisplayCoordinatorListener> default_display_coordinator_listener() {
    return default_display_coordinator_listener_;
  }

  // For testing.
  void SetDefaultDisplayForTests(std::shared_ptr<Display> display) {
    default_display_ = std::move(display);
  }

 private:
  void OnDisplaysChanged(std::vector<fuchsia_hardware_display::Info> added,
                         std::vector<fuchsia_hardware_display_types::DisplayId> removed);
  void OnClientOwnershipChange(bool has_ownership);
  void OnVsync(fuchsia_hardware_display_types::DisplayId display_id, zx::time timestamp,
               fuchsia_hardware_display::ConfigStamp applied_config_stamp,
               fuchsia_hardware_display::VsyncAckCookie cookie);

  // Must outlive `default_display_coordinator_`.
  std::shared_ptr<fidl::SyncClient<fuchsia_hardware_display::Coordinator>>
      default_display_coordinator_;
  std::shared_ptr<display::DisplayCoordinatorListener> default_display_coordinator_listener_;

  std::shared_ptr<Display> default_display_;

  // When new displays are detected, ignore all displays which don't match this ID.
  // TODO(https://fxbug.dev/42156949): Remove this when we have proper multi-display support.
  const std::optional<fuchsia_hardware_display_types::DisplayId> i_can_haz_display_id_;

  // When a new display is picked, use display mode with this index.
  // TODO(https://fxbug.dev/42156949): Remove this when we have proper multi-display support.
  const std::optional<size_t> display_mode_index_override_;

  const DisplayModeConstraints display_mode_constraints_;

  fit::closure display_available_cb_;
  // A boolean indicating whether or not we have ownership of the display
  // coordinator (not just individual displays). The default is no.
  bool owns_display_coordinator_ = false;

  FXL_DISALLOW_COPY_AND_ASSIGN(DisplayManager);
};

}  // namespace display
}  // namespace scenic_impl

#endif  // SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_MANAGER_H_
