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

#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/mode-and-id.h"
#include "src/graphics/display/lib/api-types/cpp/mode-id.h"
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
    cpp20::span<const display::PixelFormat> pixel_formats) {
  ZX_DEBUG_ASSERT(preferred_modes.size() == 1);
  ZX_DEBUG_ASSERT(pixel_formats.size() == 1);

  ZX_DEBUG_ASSERT(preferred_modes[0].id() == display::ModeId(1));

  const display_mode_t banjo_preferred_mode = preferred_modes[0].mode().ToBanjo();
  const cpp20::span<const display_mode_t> banjo_preferred_modes(&banjo_preferred_mode, 1);

  const fuchsia_images2_pixel_format_enum_value_t banjo_pixel_format = pixel_formats[0].ToBanjo();
  const cpp20::span<const fuchsia_images2_pixel_format_enum_value_t> banjo_pixel_formats(
      &banjo_pixel_format, 1);

  const raw_display_info_t banjo_display_info = {
      .display_id = display::ToBanjoDisplayId(display_id),
      .preferred_modes_list = banjo_preferred_modes.data(),
      .preferred_modes_count = banjo_preferred_modes.size(),
      .edid_bytes_list = nullptr,
      .edid_bytes_count = 0,
      .pixel_formats_list = banjo_pixel_formats.data(),
      .pixel_formats_count = banjo_pixel_formats.size(),
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
    FDF_LOG(WARNING, "OnDisplayAdded() emitted with invalid event listener; event dropped");
    return;
  }
  display_engine_listener_.OnDisplayRemoved(banjo_display_id);
}

void DisplayEngineEventsBanjo::OnDisplayVsync(display::DisplayId display_id, zx::time timestamp,
                                              display::ConfigStamp config_stamp) {
  const uint64_t banjo_display_id = display::ToBanjoDisplayId(display_id);
  const zx_time_t banjo_timestamp = timestamp.get();
  const config_stamp_t banjo_config_stamp = display::ToBanjoConfigStamp(config_stamp);

  std::lock_guard event_lock(event_mutex_);
  if (!display_engine_listener_.is_valid()) {
    FDF_LOG(WARNING, "OnDisplayAdded() emitted with invalid event listener; event dropped");
    return;
  }
  display_engine_listener_.OnDisplayVsync(banjo_display_id, banjo_timestamp, &banjo_config_stamp);
}

void DisplayEngineEventsBanjo::OnCaptureComplete() {
  std::lock_guard event_lock(event_mutex_);
  if (!display_engine_listener_.is_valid()) {
    FDF_LOG(WARNING, "OnDisplayAdded() emitted with invalid event listener; event dropped");
    return;
  }
  display_engine_listener_.OnCaptureComplete();
}

}  // namespace display
