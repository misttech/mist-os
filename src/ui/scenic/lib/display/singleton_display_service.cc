// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/singleton_display_service.h"

#include "fuchsia/ui/composition/internal/cpp/fidl.h"

namespace scenic_impl {
namespace display {

SingletonDisplayService::SingletonDisplayService(std::shared_ptr<display::Display> display)
    : display_(std::move(display)) {}

void SingletonDisplayService::GetMetrics(
    fuchsia::ui::display::singleton::Info::GetMetricsCallback callback) {
  const glm::vec2 dpr = display_->device_pixel_ratio();
  if (dpr.x != dpr.y) {
    FX_LOGS(WARNING) << "SingletonDisplayService::GetMetrics(): x/y display pixel ratio mismatch ("
                     << dpr.x << " vs. " << dpr.y << ")";
  }

  auto metrics = ::fuchsia::ui::display::singleton::Metrics();
  metrics.set_extent_in_px(
      ::fuchsia::math::SizeU{.width = display_->width_in_px(), .height = display_->height_in_px()});
  metrics.set_extent_in_mm(
      ::fuchsia::math::SizeU{.width = display_->width_in_mm(), .height = display_->height_in_mm()});
  metrics.set_recommended_device_pixel_ratio(::fuchsia::math::VecF{.x = dpr.x, .y = dpr.y});

  callback(std::move(metrics));
}

void SingletonDisplayService::GetEvent(
    fuchsia::ui::composition::internal::DisplayOwnership::GetEventCallback callback) {
  // These constants are defined as raw hex in the FIDL file, so we confirm here that they are the
  // same values as the expected constants in the ZX headers.
  static_assert(fuchsia::ui::composition::internal::SIGNAL_DISPLAY_NOT_OWNED == ZX_USER_SIGNAL_0,
                "Bad constant");
  static_assert(fuchsia::ui::composition::internal::SIGNAL_DISPLAY_OWNED == ZX_USER_SIGNAL_1,
                "Bad constant");

  zx::event dup;
  if (display_->ownership_event().duplicate(ZX_RIGHTS_BASIC, &dup) != ZX_OK) {
    FX_LOGS(ERROR) << "Display ownership event duplication error.";
    callback(zx::event());
  } else {
    callback(std::move(dup));
  }
}

void SingletonDisplayService::AddPublicService(sys::OutgoingDirectory* outgoing_directory) {
  FX_DCHECK(outgoing_directory);
  outgoing_directory->AddPublicService(info_bindings_.GetHandler(this));
  outgoing_directory->AddPublicService(ownership_bindings_.GetHandler(this));
}

}  // namespace display
}  // namespace scenic_impl
