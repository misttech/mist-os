// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-banjo.h"

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/time.h>
#include <zircon/assert.h>
#include <zircon/time.h>

#include <cstdint>
#include <mutex>

#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/mode-and-id.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"

namespace display {

DisplayEngineEventsBanjo::DisplayEngineEventsBanjo() = default;
DisplayEngineEventsBanjo::~DisplayEngineEventsBanjo() = default;

void DisplayEngineEventsBanjo::SetListener(
    const display_engine_listener_protocol_t* display_engine_listener) {
  std::lock_guard event_lock(event_mutex_);
  if (display_engine_listener == nullptr) {
    display_engine_listener_.clear();
    return;
  }

  display_engine_listener_ = ddk::DisplayEngineListenerProtocolClient(display_engine_listener);
}

void DisplayEngineEventsBanjo::OnDisplayAdded(
    display::DisplayId display_id, cpp20::span<const display::ModeAndId> preferred_modes,
    cpp20::span<const uint8_t> edid_bytes, cpp20::span<const display::PixelFormat> pixel_formats) {
  ZX_DEBUG_ASSERT(preferred_modes.size() <= kMaxPreferredModes);
  ZX_DEBUG_ASSERT(pixel_formats.size() <= kMaxPixelFormats);

  std::array<display_mode_t, kMaxPreferredModes> banjo_preferred_modes_buffer;
  for (size_t i = 0; i < preferred_modes.size(); ++i) {
    banjo_preferred_modes_buffer[i] = preferred_modes[i].mode().ToBanjo();
  }

  std::array<fuchsia_images2_pixel_format_enum_value_t, kMaxPixelFormats>
      banjo_pixel_formats_buffer;
  for (size_t i = 0; i < pixel_formats.size(); ++i) {
    banjo_pixel_formats_buffer[i] = pixel_formats[i].ToBanjo();
  }

  const raw_display_info_t banjo_display_info = {
      .display_id = display::ToBanjoDisplayId(display_id),
      .preferred_modes_list = banjo_preferred_modes_buffer.data(),
      .preferred_modes_count = preferred_modes.size(),
      .edid_bytes_list = edid_bytes.data(),
      .edid_bytes_count = edid_bytes.size(),
      .pixel_formats_list = banjo_pixel_formats_buffer.data(),
      .pixel_formats_count = pixel_formats.size(),
  };

  std::lock_guard event_lock(event_mutex_);
  if (!display_engine_listener_.is_valid()) {
    FDF_LOG(WARNING, "OnDisplayAdded() emitted with invalid event listener; event dropped");
    return;
  }
  display_engine_listener_.OnDisplayAdded(&banjo_display_info);
}

void DisplayEngineEventsBanjo::OnDisplayRemoved(display::DisplayId display_id) {
  const uint64_t banjo_display_id = display::ToBanjoDisplayId(display_id);

  std::lock_guard event_lock(event_mutex_);
  if (!display_engine_listener_.is_valid()) {
    FDF_LOG(WARNING, "OnDisplayRemoved() emitted with invalid event listener; event dropped");
    return;
  }
  display_engine_listener_.OnDisplayRemoved(banjo_display_id);
}

void DisplayEngineEventsBanjo::OnDisplayVsync(display::DisplayId display_id, zx::time timestamp,
                                              display::DriverConfigStamp config_stamp) {
  const uint64_t banjo_display_id = display::ToBanjoDisplayId(display_id);
  const zx_time_t banjo_timestamp = timestamp.get();
  const config_stamp_t banjo_driver_config_stamp = display::ToBanjoDriverConfigStamp(config_stamp);

  std::lock_guard event_lock(event_mutex_);
  if (!display_engine_listener_.is_valid()) {
    FDF_LOG(WARNING, "OnDisplayVsync() emitted with invalid event listener; event dropped");
    return;
  }
  display_engine_listener_.OnDisplayVsync(banjo_display_id, banjo_timestamp,
                                          &banjo_driver_config_stamp);
}

void DisplayEngineEventsBanjo::OnCaptureComplete() {
  std::lock_guard event_lock(event_mutex_);
  if (!display_engine_listener_.is_valid()) {
    FDF_LOG(WARNING, "OnCaptureComplete() emitted with invalid event listener; event dropped");
    return;
  }
  display_engine_listener_.OnCaptureComplete();
}

}  // namespace display
