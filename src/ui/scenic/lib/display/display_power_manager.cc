// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_power_manager.h"

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display.types/cpp/hlcpp_conversion.h>
#include <fuchsia/hardware/display/types/cpp/fidl.h>
#include <fuchsia/ui/display/internal/cpp/fidl.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include "src/ui/scenic/lib/display/display_manager.h"

namespace scenic_impl::display {

namespace {

using SetDisplayPowerResult = fuchsia::ui::display::internal::DisplayPower_SetDisplayPower_Result;

constexpr char kDisplayPowerEvents[] = "display_power_events";
constexpr char kDisplayPowerOnEvent[] = "on";
constexpr char kDisplayPowerOffEvent[] = "off";
constexpr uint64_t kInspectHistorySize = 64;

}  // namespace

DisplayPowerManager::DisplayPowerManager(DisplayManager& display_manager,
                                         inspect::Node& parent_node)
    : display_manager_(display_manager),
      inspect_display_power_events_(parent_node.CreateChild(kDisplayPowerEvents),
                                    kInspectHistorySize) {}

void DisplayPowerManager::SetDisplayPower(bool power_on, SetDisplayPowerCallback callback) {
  // No display
  if (!display_manager_.default_display()) {
    callback(SetDisplayPowerResult::WithErr(ZX_ERR_NOT_FOUND));
    return;
  }

  // TODO(https://fxbug.dev/42177175): Since currently Scenic only supports one display,
  // the DisplayPowerManager will only control power of the default display.
  // Once Scenic and DisplayManager supports multiple displays, this needs to
  // be updated to control power of all available displays.
  FX_DCHECK(display_manager_.default_display_coordinator());
  fuchsia::hardware::display::types::DisplayId id =
      fidl::NaturalToHLCPP(display_manager_.default_display()->display_id());

  fuchsia::hardware::display::Coordinator_SetDisplayPower_Result set_display_power_result;
  auto status = (*display_manager_.default_display_coordinator())
                    ->SetDisplayPower(id, power_on, &set_display_power_result);
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to call FIDL SetDisplayPower(): " << zx_status_get_string(status);
    callback(SetDisplayPowerResult::WithErr(ZX_ERR_INTERNAL));
    return;
  }
  if (set_display_power_result.is_err()) {
    FX_LOGS(WARNING) << "DisplayCoordinator SetDisplayPower() is not supported; error status: "
                     << zx_status_get_string(set_display_power_result.err());
    callback(SetDisplayPowerResult::WithErr(ZX_ERR_NOT_SUPPORTED));
    return;
  }

  inspect_display_power_events_.CreateEntry([power_on](inspect::Node& n) {
    n.RecordInt(power_on ? kDisplayPowerOnEvent : kDisplayPowerOffEvent, zx_clock_get_monotonic());
  });
  callback(SetDisplayPowerResult::WithResponse({}));
}

}  // namespace scenic_impl::display
