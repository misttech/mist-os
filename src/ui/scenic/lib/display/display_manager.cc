// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_manager.h"

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fuchsia/ui/composition/internal/cpp/fidl.h>
#include <fuchsia/ui/scenic/cpp/fidl.h>
#include <lib/fit/function.h>
#include <lib/syslog/cpp/macros.h>

namespace scenic_impl {
namespace display {

namespace {

std::optional<size_t> PickFirstDisplayModeSatisfyingConstraints(
    cpp20::span<const fuchsia_hardware_display_types::wire::Mode> modes,
    const DisplayModeConstraints& constraints) {
  for (size_t i = 0; i < modes.size(); ++i) {
    if (constraints.ModeSatisfiesConstraints(modes[i])) {
      return std::make_optional(i);
    }
  }
  return std::nullopt;
}

}  // namespace

bool DisplayModeConstraints::ModeSatisfiesConstraints(
    const fuchsia_hardware_display_types::wire::Mode& mode) const {
  if (!width_px_range.Contains(static_cast<int>(mode.active_area.width))) {
    return false;
  }
  if (!height_px_range.Contains(static_cast<int>(mode.active_area.height))) {
    return false;
  }
  if (!refresh_rate_millihertz_range.Contains(static_cast<int>(mode.refresh_rate_millihertz))) {
    return false;
  }
  return true;
}

DisplayManager::DisplayManager(fit::closure display_available_cb)
    : DisplayManager(std::nullopt, std::nullopt, /*display_mode_constraints=*/{},
                     std::move(display_available_cb)) {}

DisplayManager::DisplayManager(
    std::optional<fuchsia_hardware_display_types::wire::DisplayId> i_can_haz_display_id,
    std::optional<size_t> display_mode_index_override,
    DisplayModeConstraints display_mode_constraints, fit::closure display_available_cb)
    : i_can_haz_display_id_(i_can_haz_display_id),
      display_mode_index_override_(display_mode_index_override),
      display_mode_constraints_(std::move(display_mode_constraints)),
      display_available_cb_(std::move(display_available_cb)) {}

void DisplayManager::BindDefaultDisplayCoordinator(
    async_dispatcher_t* dispatcher,
    fidl::ClientEnd<fuchsia_hardware_display::Coordinator> coordinator,
    fidl::ServerEnd<fuchsia_hardware_display::CoordinatorListener> coordinator_listener) {
  FX_DCHECK(!default_display_coordinator_);
  FX_DCHECK(coordinator.is_valid());
  default_display_coordinator_ =
      std::make_shared<fidl::WireSharedClient<fuchsia_hardware_display::Coordinator>>(
          std::move(coordinator), dispatcher);
  default_display_coordinator_listener_ = std::make_shared<display::DisplayCoordinatorListener>(
      std::move(coordinator_listener), fit::bind_member<&DisplayManager::OnDisplaysChanged>(this),
      fit::bind_member<&DisplayManager::OnVsync>(this),
      fit::bind_member<&DisplayManager::OnClientOwnershipChange>(this));

  fidl::OneWayStatus enable_vsync_result =
      default_display_coordinator_->sync()->SetVsyncEventDelivery(true);
  if (!enable_vsync_result.ok()) {
    FX_LOGS(ERROR) << "Failed to enable vsync, status: " << enable_vsync_result.error();
  }
}

void DisplayManager::OnDisplaysChanged(
    fidl::VectorView<fuchsia_hardware_display::wire::Info> added,
    fidl::VectorView<fuchsia_hardware_display_types::wire::DisplayId> removed) {
  for (fuchsia_hardware_display::wire::Info& display : added) {
    // Ignore display if |i_can_haz_display_id| is set and it doesn't match ID.
    if (i_can_haz_display_id_.has_value() && display.id.value != i_can_haz_display_id_->value) {
      FX_LOGS(INFO) << "Ignoring display with id=" << display.id.value
                    << " ... waiting for display with id=" << i_can_haz_display_id_->value;
      continue;
    }

    if (!default_display_) {
      size_t mode_index = 0;

      // Set display mode if requested.
      if (display_mode_index_override_.has_value()) {
        if (*display_mode_index_override_ < display.modes.count()) {
          mode_index = *display_mode_index_override_;
        } else {
          FX_LOGS(ERROR) << "Requested display mode=" << *display_mode_index_override_
                         << " doesn't exist for display with id=" << display.id.value;
        }
      } else {
        std::optional<size_t> mode_index_satisfying_constraints =
            PickFirstDisplayModeSatisfyingConstraints(display.modes, display_mode_constraints_);

        // TODO(https://fxbug.dev/42097581): handle this more robustly.
        FX_CHECK(mode_index_satisfying_constraints.has_value())
            << "Failed to find a display mode satisfying all display constraints for "
               "display with id="
            << display.id.value;

        mode_index = *mode_index_satisfying_constraints;
      }

      if (mode_index != 0) {
        [[maybe_unused]] fidl::OneWayStatus set_display_mode_result =
            default_display_coordinator_->sync()->SetDisplayMode(display.id,
                                                                 display.modes[mode_index]);
        [[maybe_unused]] fidl::OneWayStatus apply_config_result =
            default_display_coordinator_->sync()->ApplyConfig();
      }

      const fuchsia_hardware_display_types::wire::Mode& mode = display.modes[mode_index];
      std::vector<fuchsia_images2::PixelFormat> pixel_formats(display.pixel_format.begin(),
                                                              display.pixel_format.end());
      default_display_ = std::make_unique<Display>(
          display.id, mode.active_area.width, mode.active_area.height, display.horizontal_size_mm,
          display.vertical_size_mm, std::move(pixel_formats), mode.refresh_rate_millihertz);
      OnClientOwnershipChange(owns_display_coordinator_);

      if (display_available_cb_) {
        display_available_cb_();
        display_available_cb_ = nullptr;
      }
    }
  }

  for (const fuchsia_hardware_display_types::wire::DisplayId& id : removed) {
    if (default_display_ && default_display_->display_id().value == id.value) {
      // TODO(https://fxbug.dev/42097581): handle this more robustly.
      FX_CHECK(false) << "Display disconnected";
      return;
    }
  }
}

void DisplayManager::OnClientOwnershipChange(bool has_ownership) {
  owns_display_coordinator_ = has_ownership;
  if (default_display_) {
    if (has_ownership) {
      default_display_->ownership_event().signal(
          fuchsia::ui::composition::internal::SIGNAL_DISPLAY_NOT_OWNED,
          fuchsia::ui::composition::internal::SIGNAL_DISPLAY_OWNED);
    } else {
      default_display_->ownership_event().signal(
          fuchsia::ui::composition::internal::SIGNAL_DISPLAY_OWNED,
          fuchsia::ui::composition::internal::SIGNAL_DISPLAY_NOT_OWNED);
    }
  }
}

void DisplayManager::OnVsync(fuchsia_hardware_display_types::wire::DisplayId display_id,
                             zx::time timestamp,
                             fuchsia_hardware_display::wire::ConfigStamp applied_config_stamp,
                             fuchsia_hardware_display::wire::VsyncAckCookie cookie) {
  if (cookie.value != fuchsia_hardware_display_types::kInvalidDispId) {
    [[maybe_unused]] fidl::OneWayStatus acknowledge_vsync_result =
        default_display_coordinator_->sync()->AcknowledgeVsync(cookie.value);
  }

  if (!default_display_) {
    return;
  }
  if (default_display_->display_id().value != display_id.value) {
    return;
  }
  default_display_->OnVsync(timestamp, applied_config_stamp);
}

}  // namespace display
}  // namespace scenic_impl
