// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-gpu-display/display-coordinator-events-banjo.h"

#include <zircon/assert.h>
#include <zircon/time.h>

#include <cstdint>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <fbl/vector.h>

#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"

namespace virtio_display {

DisplayCoordinatorEventsBanjo::DisplayCoordinatorEventsBanjo() = default;
DisplayCoordinatorEventsBanjo::~DisplayCoordinatorEventsBanjo() = default;

void DisplayCoordinatorEventsBanjo::RegisterDisplayEngineListener(
    const display_engine_listener_protocol_t* display_engine_listener) {
  fbl::AutoLock event_lock(&event_mutex_);
  if (display_engine_listener == nullptr) {
    display_engine_listener = {};
    return;
  }

  display_engine_listener_ = *display_engine_listener;
}

void DisplayCoordinatorEventsBanjo::OnDisplayAdded(const raw_display_info_t& added_display_args) {
  fbl::AutoLock event_lock(&event_mutex_);
  if (display_engine_listener_.ops == nullptr) {
    return;
  }
  display_engine_listener_on_display_added(&display_engine_listener_, &added_display_args);
}

void DisplayCoordinatorEventsBanjo::OnDisplayRemoved(display::DisplayId display_id) {
  const uint64_t banjo_display_id = display::ToBanjoDisplayId(display_id);

  fbl::AutoLock event_lock(&event_mutex_);
  if (display_engine_listener_.ops == nullptr) {
    return;
  }
  display_engine_listener_on_display_removed(&display_engine_listener_, banjo_display_id);
}

void DisplayCoordinatorEventsBanjo::OnDisplayVsync(display::DisplayId display_id,
                                                   zx::time timestamp,
                                                   display::ConfigStamp config_stamp) {
  const uint64_t banjo_display_id = display::ToBanjoDisplayId(display_id);
  const zx_time_t banjo_timestamp = timestamp.get();
  const config_stamp_t banjo_config_stamp = display::ToBanjoConfigStamp(config_stamp);

  fbl::AutoLock event_lock(&event_mutex_);
  if (display_engine_listener_.ops == nullptr) {
    return;
  }
  display_engine_listener_on_display_vsync(&display_engine_listener_, banjo_display_id,
                                           banjo_timestamp, &banjo_config_stamp);
}

void DisplayCoordinatorEventsBanjo::OnCaptureComplete() {
  fbl::AutoLock event_lock(&event_mutex_);
  if (display_engine_listener_.ops == nullptr) {
    return;
  }
  display_engine_listener_on_capture_complete(&display_engine_listener_);
}

}  // namespace virtio_display
