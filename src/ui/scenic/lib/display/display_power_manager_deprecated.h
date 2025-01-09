// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_POWER_MANAGER_DEPRECATED_H_
#define SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_POWER_MANAGER_DEPRECATED_H_

#include <fuchsia/ui/display/internal/cpp/fidl.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/fit/function.h>
#include <lib/inspect/contrib/cpp/bounded_list_node.h>
#include <lib/inspect/cpp/inspect.h>

#include <optional>

#include "src/lib/fxl/macros.h"
#include "src/ui/scenic/lib/display/display.h"
#include "src/ui/scenic/lib/display/display_coordinator_listener.h"
#include "src/ui/scenic/lib/display/display_manager.h"

namespace scenic_impl::display {

// Implements the |fuchsia::ui::display::internal::DisplayPower| protocol,
// Internal protocol clients are able to control the power of all available
// display devices through this protocol.
class DisplayPowerManagerDeprecated : public fuchsia::ui::display::internal::DisplayPower {
 public:
  DisplayPowerManagerDeprecated(DisplayManager& display_manager, inspect::Node& parent_node);

  // |fuchsia::ui::display::internal::DisplayPower|
  void SetDisplayPower(bool power_on, SetDisplayPowerCallback callback) override;

  fidl::InterfaceRequestHandler<DisplayPower> GetHandler() { return bindings_.GetHandler(this); }

 private:
  DisplayManager& display_manager_;
  inspect::contrib::BoundedListNode inspect_display_power_events_;
  fidl::BindingSet<fuchsia::ui::display::internal::DisplayPower> bindings_;

  FXL_DISALLOW_COPY_AND_ASSIGN(DisplayPowerManagerDeprecated);
};

}  // namespace scenic_impl::display

#endif  // SRC_UI_SCENIC_LIB_DISPLAY_DISPLAY_POWER_MANAGER_DEPRECATED_H_
