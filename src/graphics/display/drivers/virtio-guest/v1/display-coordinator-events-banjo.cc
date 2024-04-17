// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v1/display-coordinator-events-banjo.h"

#include <zircon/assert.h>
#include <zircon/time.h>

#include <cstdint>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <fbl/vector.h>

#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"

namespace virtio_display {

DisplayCoordinatorEventsBanjo::DisplayCoordinatorEventsBanjo() = default;
DisplayCoordinatorEventsBanjo::~DisplayCoordinatorEventsBanjo() = default;

void DisplayCoordinatorEventsBanjo::SetDisplayControllerInterface(
    const display_controller_interface_protocol_t* display_controller_interface) {
  fbl::AutoLock event_lock(&event_mutex_);
  if (display_controller_interface == nullptr) {
    display_controller_interface = {};
    return;
  }

  display_controller_interface_ = *display_controller_interface;
}

void DisplayCoordinatorEventsBanjo::OnDisplaysChanged(
    cpp20::span<const added_display_args_t> added_displays,
    cpp20::span<const display::DisplayId> removed_display_ids) {
  fbl::Vector<uint64_t> banjo_removed_display_ids;

  fbl::AllocChecker alloc_checker;
  banjo_removed_display_ids.reserve(removed_display_ids.size(), &alloc_checker);
  if (!alloc_checker.check()) {
    return;
  }

  for (display::DisplayId removed_display_id : removed_display_ids) {
    banjo_removed_display_ids.push_back(display::ToBanjoDisplayId(removed_display_id),
                                        &alloc_checker);
    ZX_DEBUG_ASSERT(banjo_removed_display_ids.size() <= removed_display_ids.size());
    ZX_DEBUG_ASSERT_MSG(alloc_checker.check(),
                        "push_back() failed despite having the required capacity reserve()d");
  }

  fbl::AutoLock event_lock(&event_mutex_);
  if (display_controller_interface_.ops == nullptr) {
    return;
  }
  display_controller_interface_on_displays_changed(
      &display_controller_interface_, added_displays.data(), added_displays.size(),
      banjo_removed_display_ids.data(), banjo_removed_display_ids.size());
}

void DisplayCoordinatorEventsBanjo::OnDisplayVsync(display::DisplayId display_id,
                                                   zx::time timestamp,
                                                   display::ConfigStamp config_stamp) {
  const uint64_t banjo_display_id = display::ToBanjoDisplayId(display_id);
  const zx_time_t banjo_timestamp = timestamp.get();
  const config_stamp_t banjo_config_stamp = display::ToBanjoConfigStamp(config_stamp);

  fbl::AutoLock event_lock(&event_mutex_);
  if (display_controller_interface_.ops == nullptr) {
    return;
  }
  display_controller_interface_on_display_vsync(&display_controller_interface_, banjo_display_id,
                                                banjo_timestamp, &banjo_config_stamp);
}

void DisplayCoordinatorEventsBanjo::OnCaptureComplete() {
  fbl::AutoLock event_lock(&event_mutex_);
  if (display_controller_interface_.ops == nullptr) {
    return;
  }
  display_controller_interface_on_capture_complete(&display_controller_interface_);
}

}  // namespace virtio_display
