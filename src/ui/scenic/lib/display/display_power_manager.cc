// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_power_manager.h"

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fuchsia/ui/display/internal/cpp/fidl.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include "src/ui/scenic/lib/display/display_manager.h"
#include "src/ui/scenic/lib/display/util.h"

namespace scenic_impl::display {

namespace {

using DisplayPowerSetDisplayPowerResponse =
    fuchsia_ui_display_singleton::DisplayPowerSetDisplayPowerResponse;

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

void DisplayPowerManager::SetDisplayPower(SetDisplayPowerRequest& request,
                                          SetDisplayPowerCompleter::Sync& completer) {
  SetDisplayPower(request.power_on(), [completer = completer.ToAsync()](auto result) mutable {
    completer.Reply(result);
  });
}

void DisplayPowerManager::SetDisplayPower(bool power_on,
                                          fit::function<void(fit::result<zx_status_t>)> completer) {
  // No display
  if (!display_manager_.default_display()) {
    completer(fit::error(ZX_ERR_NOT_FOUND));
    return;
  }

  // TODO(https://fxbug.dev/42177175): Since currently Scenic only supports one display,
  // the DisplayPowerManager will only control power of the default display.
  // Once Scenic and DisplayManager supports multiple displays, this needs to
  // be updated to control power of all available displays.
  std::shared_ptr<fidl::SyncClient<fuchsia_hardware_display::Coordinator>> coordinator =
      display_manager_.default_display_coordinator();
  FX_DCHECK(coordinator);
  fuchsia_hardware_display_types::DisplayId id = display_manager_.default_display()->display_id();

  fit::result set_display_power_result = (*coordinator)
                                             ->SetDisplayPower({{
                                                 .display_id = id,
                                                 .power_on = power_on,
                                             }});
  if (set_display_power_result.is_error()) {
    const auto& error_value = set_display_power_result.error_value();
    if (error_value.is_framework_error()) {
      FX_LOGS(ERROR) << "Failed to call FIDL SetDisplayPower(): "
                     << set_display_power_result.error_value();
      completer(fit::error(ZX_ERR_INTERNAL));
      return;
    }

    // error_value.is_domain_error()
    FX_LOGS(WARNING) << "DisplayCoordinator SetDisplayPower() is not supported; error status: "
                     << set_display_power_result.error_value();
    completer(fit::error(ZX_ERR_NOT_SUPPORTED));
    return;
  }

  FX_LOGS(INFO) << "Successfully set display power: power " << (power_on ? "on" : "off");
  inspect_display_power_events_.CreateEntry([power_on](inspect::Node& n) {
    n.RecordInt(power_on ? kDisplayPowerOnEvent : kDisplayPowerOffEvent, zx_clock_get_monotonic());
  });
  completer(fit::ok());
}

}  // namespace scenic_impl::display
