// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/display_manager.h"

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/hlcpp_conversion.h>
#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/ui/scenic/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>

#include "lib/fidl/cpp/comparison.h"
#include "lib/fit/function.h"

namespace scenic_impl {
namespace display {

DisplayManager::DisplayManager(fit::closure display_available_cb)
    : DisplayManager(std::nullopt, std::nullopt, std::move(display_available_cb)) {}

DisplayManager::DisplayManager(
    std::optional<fuchsia::hardware::display::DisplayId> i_can_haz_display_id,
    std::optional<uint64_t> i_can_haz_display_mode, fit::closure display_available_cb)
    : i_can_haz_display_id_(i_can_haz_display_id),
      i_can_haz_display_mode_(i_can_haz_display_mode),
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
      /*on_invalid_cb=*/nullptr, fit::bind_member<&DisplayManager::OnDisplaysChanged>(this),
      fit::bind_member<&DisplayManager::OnClientOwnershipChange>(this));

  // Set up callback to handle Vsync notifications, and ask coordinator to send these notifications.
  default_display_coordinator_listener_->SetOnVsyncCallback(
      fit::bind_member<&DisplayManager::OnVsync>(this));
  zx_status_t vsync_status = (*default_display_coordinator_)->EnableVsync(true);
  if (vsync_status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to enable vsync, status: " << vsync_status;
  }
}

void DisplayManager::OnDisplaysChanged(std::vector<fuchsia::hardware::display::Info> added,
                                       std::vector<fuchsia::hardware::display::DisplayId> removed) {
  for (auto& display : added) {
    // Ignore display if |i_can_haz_display_id| is set and it doesn't match ID.
    if (i_can_haz_display_id_.has_value() && !fidl::Equals(display.id, *i_can_haz_display_id_)) {
      FX_LOGS(INFO) << "Ignoring display with id=" << display.id.value
                    << " ... waiting for display with id=" << i_can_haz_display_id_->value;
      continue;
    }

    if (!default_display_) {
      size_t mode_idx = 0;

      // Set display mode if requested.
      if (i_can_haz_display_mode_.has_value()) {
        if (*i_can_haz_display_mode_ < display.modes.size()) {
          mode_idx = *i_can_haz_display_mode_;
          (*default_display_coordinator_)->SetDisplayMode(display.id, display.modes[mode_idx]);
          (*default_display_coordinator_)->ApplyConfig();
        } else {
          FX_LOGS(ERROR) << "Requested display mode=" << *i_can_haz_display_mode_
                         << " doesn't exist for display with id=" << display.id.value;
        }
      }

      std::vector<fuchsia_images2::PixelFormat> pixel_formats =
          fidl::HLCPPToNatural(display.pixel_format);
      auto& mode = display.modes[mode_idx];
      default_display_ = std::make_unique<Display>(
          display.id, mode.horizontal_resolution, mode.vertical_resolution,
          display.horizontal_size_mm, display.vertical_size_mm, std::move(pixel_formats));
      OnClientOwnershipChange(owns_display_coordinator_);

      if (display_available_cb_) {
        display_available_cb_();
        display_available_cb_ = nullptr;
      }
    }
  }

  for (fuchsia::hardware::display::DisplayId id : removed) {
    if (default_display_ && fidl::Equals(default_display_->display_id(), id)) {
      // TODO(fxbug.dev/23490): handle this more robustly.
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

void DisplayManager::OnVsync(fuchsia::hardware::display::DisplayId display_id, uint64_t timestamp,
                             fuchsia::hardware::display::ConfigStamp applied_config_stamp,
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
