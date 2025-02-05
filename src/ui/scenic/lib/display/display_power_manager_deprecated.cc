// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_power_manager_deprecated.h"

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fuchsia/ui/display/internal/cpp/fidl.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include "src/ui/scenic/lib/display/display_manager.h"
#include "src/ui/scenic/lib/display/util.h"

namespace scenic_impl::display {

namespace {

using SetDisplayPowerResult = fuchsia::ui::display::internal::DisplayPower_SetDisplayPower_Result;

constexpr char kDisplayPowerEvents[] = "display_power_events";
constexpr char kDisplayPowerOnEvent[] = "on";
constexpr char kDisplayPowerOffEvent[] = "off";
constexpr uint64_t kInspectHistorySize = 64;

}  // namespace

DisplayPowerManagerDeprecated::DisplayPowerManagerDeprecated(DisplayManager& display_manager,
                                                             inspect::Node& parent_node)
    : display_manager_(display_manager),
      inspect_display_power_events_(parent_node.CreateChild(kDisplayPowerEvents),
                                    kInspectHistorySize) {}

void DisplayPowerManagerDeprecated::SetDisplayPower(bool power_on,
                                                    SetDisplayPowerCallback callback) {
  // No display
  if (!display_manager_.default_display()) {
    callback(SetDisplayPowerResult::WithErr(ZX_ERR_NOT_FOUND));
    return;
  }

  // TODO(https://fxbug.dev/42177175): Since currently Scenic only supports one display,
  // the DisplayPowerManagerDeprecated will only control power of the default display.
  // Once Scenic and DisplayManager supports multiple displays, this needs to
  // be updated to control power of all available displays.
  std::shared_ptr<fidl::WireSharedClient<fuchsia_hardware_display::Coordinator>> coordinator =
      display_manager_.default_display_coordinator();
  FX_DCHECK(coordinator);
  fuchsia_hardware_display_types::wire::DisplayId id =
      display_manager_.default_display()->display_id();

  auto set_display_power_result = (*coordinator).sync()->SetDisplayPower(id, power_on);
  if (!set_display_power_result.ok()) {
    FX_LOGS(ERROR) << "Failed to call FIDL SetDisplayPower(): "
                   << set_display_power_result.status_string();
    callback(SetDisplayPowerResult::WithErr(ZX_ERR_INTERNAL));
    return;
  }

  if (set_display_power_result->is_error()) {
    FX_LOGS(WARNING) << "DisplayCoordinator SetDisplayPower() is not supported; error status: "
                     << zx_status_get_string(set_display_power_result->error_value());
    callback(SetDisplayPowerResult::WithErr(ZX_ERR_NOT_SUPPORTED));
    return;
  }

  FX_LOGS(INFO) << "Successfully set display power: power " << (power_on ? "on" : "off");
  inspect_display_power_events_.CreateEntry([power_on](inspect::Node& n) {
    n.RecordInt(power_on ? kDisplayPowerOnEvent : kDisplayPowerOffEvent, zx_clock_get_monotonic());
  });
  callback(SetDisplayPowerResult::WithResponse({}));
}

}  // namespace scenic_impl::display
