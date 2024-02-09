// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_manager.h"

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/hlcpp_conversion.h>
#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/ui/scenic/cpp/fidl.h>
#include <lib/fidl/cpp/comparison.h>
#include <lib/fidl/cpp/hlcpp_conversion.h>
#include <lib/fit/function.h>
#include <lib/syslog/cpp/macros.h>

namespace scenic_impl {
namespace display {

namespace {

static constexpr uint32_t kMillihertzPerCentihertz = 10;

std::optional<size_t> PickFirstDisplayModeSatisfyingConstraints(
    cpp20::span<const fuchsia_hardware_display::Mode> modes,
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
    const fuchsia_hardware_display::Mode& mode) const {
  if (!width_px_range.Contains(static_cast<int>(mode.horizontal_resolution()))) {
    return false;
  }
  if (!height_px_range.Contains(static_cast<int>(mode.vertical_resolution()))) {
    return false;
  }
  const int refresh_rate_millihertz =
      static_cast<int>(mode.refresh_rate_e2() * kMillihertzPerCentihertz);
  if (!refresh_rate_millihertz_range.Contains(refresh_rate_millihertz)) {
    return false;
  }
  return true;
}

DisplayManager::DisplayManager(fit::closure display_available_cb)
    : DisplayManager(std::nullopt, std::nullopt, /*display_mode_constraints=*/{},
                     std::move(display_available_cb)) {}

DisplayManager::DisplayManager(
    std::optional<fuchsia::hardware::display::types::DisplayId> i_can_haz_display_id,
    std::optional<size_t> display_mode_index_override,
    DisplayModeConstraints display_mode_constraints, fit::closure display_available_cb)
    : i_can_haz_display_id_(i_can_haz_display_id),
      display_mode_index_override_(display_mode_index_override),
      display_mode_constraints_(std::move(display_mode_constraints)),
      display_available_cb_(std::move(display_available_cb)) {}

void DisplayManager::BindDefaultDisplayCoordinator(
    fidl::ClientEnd<fuchsia_hardware_display::Coordinator> coordinator) {
  FX_DCHECK(!default_display_coordinator_);
  FX_DCHECK(coordinator.is_valid());
  default_display_coordinator_ = std::make_shared<fuchsia::hardware::display::CoordinatorSyncPtr>();
  default_display_coordinator_->Bind(coordinator.TakeChannel());
  default_display_coordinator_listener_ =
      std::make_shared<display::DisplayCoordinatorListener>(default_display_coordinator_);
  default_display_coordinator_listener_->InitializeCallbacks(
      fit::bind_member<&DisplayManager::OnDisplaysChanged>(this),
      fit::bind_member<&DisplayManager::OnClientOwnershipChange>(this));

  // Set up callback to handle Vsync notifications, and ask coordinator to send these notifications.
  default_display_coordinator_listener_->SetOnVsyncCallback(
      fit::bind_member<&DisplayManager::OnVsync>(this));
  zx_status_t vsync_status = (*default_display_coordinator_)->EnableVsync(true);
  if (vsync_status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to enable vsync, status: " << vsync_status;
  }
}

void DisplayManager::OnDisplaysChanged(
    std::vector<fuchsia::hardware::display::Info> added,
    std::vector<fuchsia::hardware::display::types::DisplayId> removed) {
  for (auto& display : added) {
    // Ignore display if |i_can_haz_display_id| is set and it doesn't match ID.
    if (i_can_haz_display_id_.has_value() && !fidl::Equals(display.id, *i_can_haz_display_id_)) {
      FX_LOGS(INFO) << "Ignoring display with id=" << display.id.value
                    << " ... waiting for display with id=" << i_can_haz_display_id_->value;
      continue;
    }

    if (!default_display_) {
      size_t mode_index = 0;

      // Set display mode if requested.
      if (display_mode_index_override_.has_value()) {
        if (*display_mode_index_override_ < display.modes.size()) {
          mode_index = *display_mode_index_override_;
        } else {
          FX_LOGS(ERROR) << "Requested display mode=" << *display_mode_index_override_
                         << " doesn't exist for display with id=" << display.id.value;
        }
      } else {
        std::vector<fuchsia_hardware_display::Mode> natural_modes =
            fidl::HLCPPToNatural(display.modes);
        std::optional<size_t> mode_index_satisfying_constraints =
            PickFirstDisplayModeSatisfyingConstraints(natural_modes, display_mode_constraints_);

        // TODO(https://fxbug.dev/42097581): handle this more robustly.
        FX_CHECK(mode_index_satisfying_constraints.has_value())
            << "Failed to find a display mode satisfying all display constraints for "
               "display with id="
            << display.id.value;

        mode_index = *mode_index_satisfying_constraints;
      }

      if (mode_index != 0) {
        (*default_display_coordinator_)
            ->SetDisplayMode(
                fuchsia::hardware::display::types::DisplayId{.value = display.id.value},
                display.modes[mode_index]);
        (*default_display_coordinator_)->ApplyConfig();
      }

      std::vector<fuchsia_images2::PixelFormat> pixel_formats =
          fidl::HLCPPToNatural(display.pixel_format);
      const fuchsia::hardware::display::Mode& mode = display.modes[mode_index];
      default_display_ = std::make_unique<Display>(
          display.id, mode.horizontal_resolution, mode.vertical_resolution,
          display.horizontal_size_mm, display.vertical_size_mm, std::move(pixel_formats),
          mode.refresh_rate_e2 * kMillihertzPerCentihertz);
      OnClientOwnershipChange(owns_display_coordinator_);

      if (display_available_cb_) {
        display_available_cb_();
        display_available_cb_ = nullptr;
      }
    }
  }

  for (fuchsia::hardware::display::types::DisplayId id : removed) {
    if (default_display_ && fidl::Equals(default_display_->display_id(), id)) {
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
      default_display_->ownership_event().signal(fuchsia::ui::scenic::displayNotOwnedSignal,
                                                 fuchsia::ui::scenic::displayOwnedSignal);
    } else {
      default_display_->ownership_event().signal(fuchsia::ui::scenic::displayOwnedSignal,
                                                 fuchsia::ui::scenic::displayNotOwnedSignal);
    }
  }
}

void DisplayManager::SetVsyncCallback(VsyncCallback callback) {
  FX_DCHECK(!(static_cast<bool>(callback) && static_cast<bool>(vsync_callback_)))
      << "cannot stomp vsync callback.";

  vsync_callback_ = std::move(callback);
}

void DisplayManager::OnVsync(fuchsia::hardware::display::types::DisplayId display_id,
                             uint64_t timestamp,
                             fuchsia::hardware::display::types::ConfigStamp applied_config_stamp,
                             uint64_t cookie) {
  if (cookie) {
    (*default_display_coordinator_)->AcknowledgeVsync(cookie);
  }

  if (vsync_callback_) {
    vsync_callback_(display_id, zx::time(timestamp), applied_config_stamp);
  }

  if (!default_display_) {
    return;
  }
  if (!fidl::Equals(default_display_->display_id(), display_id)) {
    return;
  }
  default_display_->OnVsync(zx::time(timestamp), applied_config_stamp);
}

}  // namespace display
}  // namespace scenic_impl
