// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_CONTROLLER_H_
#define SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_CONTROLLER_H_

#include <fidl/fuchsia.images2/cpp/fidl.h>
#include <fuchsia/hardware/display/cpp/fidl.h>
#include <lib/fit/function.h>

#include <cstdint>
#include <memory>

namespace scenic_impl {
namespace display {

class DisplayCoordinator;
class Display2;
class DisplayManager;
class DisplayManager2;

namespace test {
class DisplayCoordinatorTest_DisplayCoordinatorTest_Test;
}

using DisplayCoordinatorUniquePtr =
    std::unique_ptr<DisplayCoordinator, std::function<void(DisplayCoordinator*)>>;
using OnDisplayRemovedCallback = fit::function<void(fuchsia::hardware::display::DisplayId)>;
using OnDisplayAddedCallback = fit::function<void(Display2*)>;
using OnVsyncCallback =
    fit::function<void(zx::time timestamp, fuchsia::hardware::display::ConfigStamp config_stamp)>;

// Display metadata, as well as a registration point for vsync events for the display.
class Display2 {
 public:
  Display2(fuchsia::hardware::display::DisplayId display_id,
           std::vector<fuchsia::hardware::display::Mode> display_modes,
           std::vector<fuchsia_images2::PixelFormat> pixel_formats);

  // The display's ID in the context of DisplayManager's DisplayCoordinator.
  fuchsia::hardware::display::DisplayId display_id() const { return display_id_; }

  const std::vector<fuchsia::hardware::display::Mode>& display_modes() const {
    return display_modes_;
  }
  const std::vector<fuchsia_images2::PixelFormat>& pixel_formats() const { return pixel_formats_; }
  void set_on_vsync_callback(OnVsyncCallback on_vsync_callback) {
    on_vsync_callback_ = std::move(on_vsync_callback);
  }

  // Invokes Vsync callback. Should only be called by DisplayManager or during testing.
  void OnVsync(zx::time timestamp, fuchsia::hardware::display::ConfigStamp config_stamp);

 private:
  fuchsia::hardware::display::DisplayId display_id_;
  std::vector<fuchsia::hardware::display::Mode> display_modes_;
  std::vector<fuchsia_images2::PixelFormat> pixel_formats_;
  OnVsyncCallback on_vsync_callback_;
};

// Wraps a display coordinator interface, and provides a live-updated list of displays
// attached to the display coordinator.
class DisplayCoordinator {
 public:
  DisplayCoordinator(
      std::vector<Display2> displays,
      const std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr>& coordinator);

  const std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr>& coordinator() {
    return coordinator_;
  }
  std::vector<Display2>* displays() { return &displays_; }

  void set_on_display_added_callback(OnDisplayAddedCallback on_display_added) {
    on_display_added_listener_ = std::move(on_display_added);
  }

  void set_on_display_removed_callback(OnDisplayRemovedCallback on_display_removed) {
    on_display_removed_listener_ = std::move(on_display_removed);
  }

  DisplayCoordinator(const DisplayCoordinator&) = delete;
  DisplayCoordinator(DisplayCoordinator&&) = delete;
  DisplayCoordinator& operator=(const DisplayCoordinator&) = delete;
  DisplayCoordinator& operator=(DisplayCoordinator&&) = delete;

 private:
  friend class DisplayManager2;
  friend class scenic_impl::display::test::DisplayCoordinatorTest_DisplayCoordinatorTest_Test;

  // Adds a display. Should only be called by DisplayManager or during testing.
  void AddDisplay(Display2 display);

  // Removes a display. Should only be called by DisplayManager or during testing.
  bool RemoveDisplay(fuchsia::hardware::display::DisplayId display_id);

  std::vector<Display2> displays_;
  // TODO(fxbug.dev/42795): Replace with a fxl::WeakPtr.
  std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> coordinator_;
  OnDisplayRemovedCallback on_display_removed_listener_;
  OnDisplayAddedCallback on_display_added_listener_;
};

}  // namespace display
}  // namespace scenic_impl

#endif  // SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_CONTROLLER_H_
