// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_MANAGER2_H_
#define SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_MANAGER2_H_

#include <fidl/fuchsia.images2/cpp/fidl.h>
#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/ui/display/cpp/fidl.h>
#include <lib/fidl/cpp/interface_ptr_set.h>
#include <lib/zx/channel.h>

#include <vector>

#include "src/lib/fxl/macros.h"
#include "src/lib/fxl/memory/weak_ptr.h"
#include "src/ui/scenic/lib/display/display_controller.h"
#include "src/ui/scenic/lib/display/display_controller_listener.h"

namespace scenic_impl {
namespace display {

// Implements the |fuchsia::ui::display::DisplayManager| protocol. Notifies protocol clients
// of new or removed displays and allows changing of display configuration. Every display is
// associated with a DisplayRef which can also be used as a parameter to other apis (e.g. Scenic).
// Additionally, allows an internal (within Scenic) client to claim the display and
class DisplayManager2 : public fuchsia::ui::display::DisplayManager {
 public:
  DisplayManager2();

  // |fuchsia::ui::display::DisplayManager|
  void AddDisplayListener(fidl::InterfaceHandle<fuchsia::ui::display::DisplayListener>
                              display_listener_interface_handle) override;

  // Remaining methods are not part of the FIDL protocol.

  // Called by initializing code whenever a new DisplayzCoordinator is discovered, or by tests.
  void AddDisplayCoordinator(
      std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> coordinator,
      std::unique_ptr<DisplayCoordinatorListener> coordinator_listener);

  DisplayCoordinatorUniquePtr ClaimDisplay(zx_koid_t display_ref_koid);
  DisplayCoordinatorUniquePtr ClaimFirstDisplayDeprecated();

  const fxl::WeakPtr<DisplayManager2> GetWeakPtr() { return weak_factory_.GetWeakPtr(); }

  // For testing purposes only.
  const std::string& last_error() { return last_error_; }

  DisplayManager2(const DisplayManager2&) = delete;
  DisplayManager2(DisplayManager2&&) = delete;
  DisplayManager2& operator=(const DisplayManager2&) = delete;
  DisplayManager2& operator=(DisplayManager2&&) = delete;

 private:
  struct DisplayInfoPrivate {
    // |id| assigned by the DisplayCoordinator.
    uint64_t id = 0;

    zx_koid_t display_ref_koid = 0;

    std::vector<fuchsia_images2::PixelFormat> pixel_formats;

    // Interface for the DisplayCoordinator that this display is connected to.
    std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> coordinator;

    // Also stores the key version of the DisplayRef.
    fuchsia::ui::display::Info info;
  };

  // Internal data structure that holds the DisplayCoordinator interface and
  // associated info (listener, list of Displays).
  struct DisplayCoordinatorPrivate {
    // If a a client has called ClaimDisplay(), this will be non-null and point
    // to the DisplayCoordinator passed to the client. This pointer is nulled
    // out by the custom deleter for the DisplayCoordinator.
    DisplayCoordinator* claimed_dc = nullptr;

    std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> coordinator;
    std::unique_ptr<DisplayCoordinatorListener> listener;
    std::vector<DisplayInfoPrivate> displays;

    // The latest value of the OnClientOwnershipChange event from the
    // display coordinator.
    bool has_ownership = false;
  };
  using DisplayCoordinatorPrivateUniquePtr =
      std::unique_ptr<DisplayCoordinatorPrivate, std::function<void(DisplayCoordinatorPrivate*)>>;

  void RemoveOnInvalid(DisplayCoordinatorPrivate* dc);
  void OnDisplaysChanged(DisplayCoordinatorPrivate* dc,
                         std::vector<fuchsia::hardware::display::Info> displays_added,
                         std::vector<uint64_t> displays_removed);
  void OnDisplayOwnershipChanged(DisplayCoordinatorPrivate* dc, bool has_ownership);

  std::tuple<DisplayCoordinatorPrivate*, DisplayInfoPrivate*> FindDisplay(
      zx_koid_t display_ref_koid);
  DisplayCoordinatorPrivate* FindDisplayCoordinatorPrivate(DisplayCoordinator* dc);

  static DisplayInfoPrivate NewDisplayInfoPrivate(
      fuchsia::hardware::display::Info hardware_display_info,
      std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> coordinator);
  static void InvokeDisplayAddedForListener(
      const fidl::InterfacePtr<fuchsia::ui::display::DisplayListener>& listener,
      const DisplayInfoPrivate& display_info_private);
  static void InvokeDisplayOwnershipChangedForListener(
      const fidl::InterfacePtr<fuchsia::ui::display::DisplayListener>& listener,
      DisplayCoordinatorPrivate* dc, bool has_ownership);

  // Helper functions for lists of DisplayInfoPrivate.
  static bool HasDisplayWithId(const std::vector<DisplayManager2::DisplayInfoPrivate>& displays,
                               uint64_t display_id);

  static std::optional<DisplayManager2::DisplayInfoPrivate> RemoveDisplayWithId(
      std::vector<DisplayInfoPrivate>* displays, uint64_t display_id);

  std::vector<DisplayCoordinatorPrivateUniquePtr> display_coordinators_private_;
  fidl::InterfacePtrSet<fuchsia::ui::display::DisplayListener> display_listeners_;
  std::string last_error_;

  fxl::WeakPtrFactory<DisplayManager2> weak_factory_;  // must be last
};

}  // namespace display
}  // namespace scenic_impl

#endif  // SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_MANAGER2_H_
