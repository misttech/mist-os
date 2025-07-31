// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-fidl.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/driver/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/time.h>
#include <zircon/assert.h>
#include <zircon/time.h>

#include <algorithm>
#include <array>
#include <cstdint>

#include <sdk/lib/driver/logging/cpp/logger.h>

#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/mode-and-id.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"

namespace display {

DisplayEngineEventsFidl::DisplayEngineEventsFidl() = default;
DisplayEngineEventsFidl::~DisplayEngineEventsFidl() = default;

void DisplayEngineEventsFidl::SetListener(
    fdf::ClientEnd<fuchsia_hardware_display_engine::EngineListener> client_end) {
  if (client_end.is_valid()) {
    fidl_client_ =
        fdf::WireSyncClient<fuchsia_hardware_display_engine::EngineListener>(std::move(client_end));
  } else {
    fidl_client_ = fdf::WireSyncClient<fuchsia_hardware_display_engine::EngineListener>();
  }
}

void DisplayEngineEventsFidl::OnDisplayAdded(
    display::DisplayId display_id, cpp20::span<const display::ModeAndId> preferred_modes,
    cpp20::span<const uint8_t> edid_bytes, cpp20::span<const display::PixelFormat> pixel_formats) {
  ZX_DEBUG_ASSERT(preferred_modes.size() <= kMaxPreferredModes);
  ZX_DEBUG_ASSERT(pixel_formats.size() <= kMaxPixelFormats);

  if (!fidl_client_.is_valid()) {
    FDF_LOG(WARNING, "OnDisplayAdded() emitted with invalid event listener; event dropped");
    return;
  }

  std::array<fuchsia_hardware_display_types::wire::Mode, kMaxPreferredModes>
      fidl_preferred_modes_buffer;
  for (size_t i = 0; i < preferred_modes.size(); ++i) {
    fidl_preferred_modes_buffer[i] = preferred_modes[i].mode().ToFidl();
  }

  std::array<fuchsia_images2::wire::PixelFormat, kMaxPixelFormats> fidl_pixel_formats_buffer;
  for (size_t i = 0; i < pixel_formats.size(); ++i) {
    fidl_pixel_formats_buffer[i] = pixel_formats[i].ToFidl();
  }

  ZX_DEBUG_ASSERT(edid_bytes.size() <= edid_buffer_.max_size());
  std::copy(edid_bytes.begin(), edid_bytes.end(), edid_buffer_.begin());

  fuchsia_hardware_display_engine::wire::RawDisplayInfo fidl_display_info = {
      .display_id = display_id.ToFidl(),
      .preferred_modes = fidl::VectorView<fuchsia_hardware_display_types::wire::Mode>::FromExternal(
          fidl_preferred_modes_buffer.data(), preferred_modes.size()),
      .edid_bytes = fidl::VectorView<uint8_t>::FromExternal(edid_buffer_.data(), edid_bytes.size()),
      .pixel_formats = fidl::VectorView<fuchsia_images2::wire::PixelFormat>::FromExternal(
          fidl_pixel_formats_buffer.data(), pixel_formats.size()),
  };

  // TODO(https://fxbug.dev/430058446): Eliminate the per-call dynamic memory
  // allocation caused by fdf::Arena creation.
  fdf::Arena arena('DISP');
  fidl::OneWayStatus fidl_transport_status =
      fidl_client_.buffer(arena)->OnDisplayAdded(std::move(fidl_display_info));
  if (!fidl_transport_status.ok()) {
    FDF_LOG(ERROR, "OnDisplayAdded() FIDL failure: %s",
            fidl_transport_status.error().FormatDescription().c_str());
  }
}

void DisplayEngineEventsFidl::OnDisplayRemoved(display::DisplayId display_id) {
  if (!fidl_client_.is_valid()) {
    FDF_LOG(WARNING, "OnDisplayRemoved() emitted with invalid event listener; event dropped");
    return;
  }

  // TODO(https://fxbug.dev/430058446): Eliminate the per-call dynamic memory
  // allocation caused by fdf::Arena creation.
  fdf::Arena arena('DISP');
  fidl::OneWayStatus fidl_transport_status =
      fidl_client_.buffer(arena)->OnDisplayRemoved(display_id.ToFidl());
  if (!fidl_transport_status.ok()) {
    FDF_LOG(ERROR, "OnDisplayRemoved() FIDL failure: %s",
            fidl_transport_status.error().FormatDescription().c_str());
  }
}

void DisplayEngineEventsFidl::OnDisplayVsync(display::DisplayId display_id,
                                             zx::time_monotonic timestamp,
                                             display::DriverConfigStamp config_stamp) {
  if (!fidl_client_.is_valid()) {
    FDF_LOG(WARNING, "OnDisplayVsync() emitted with invalid event listener; event dropped");
    return;
  }

  // TODO(https://fxbug.dev/430058446): Eliminate the per-call dynamic memory
  // allocation caused by fdf::Arena creation.
  fdf::Arena arena('DISP');
  fidl::OneWayStatus fidl_transport_status = fidl_client_.buffer(arena)->OnDisplayVsync(
      display_id.ToFidl(), timestamp, config_stamp.ToFidl());
  if (!fidl_transport_status.ok()) {
    FDF_LOG(ERROR, "OnDisplayVsync() FIDL failure: %s",
            fidl_transport_status.error().FormatDescription().c_str());
  }
}

void DisplayEngineEventsFidl::OnCaptureComplete() {
  if (!fidl_client_.is_valid()) {
    FDF_LOG(WARNING, "OnCaptureComplete() emitted with invalid event listener; event dropped");
    return;
  }

  // TODO(https://fxbug.dev/430058446): Eliminate the per-call dynamic memory
  // allocation caused by fdf::Arena creation.
  fdf::Arena arena('DISP');
  fidl::OneWayStatus fidl_transport_status = fidl_client_.buffer(arena)->OnCaptureComplete();
  if (!fidl_transport_status.ok()) {
    FDF_LOG(ERROR, "OnCaptureComplete() FIDL failure: %s",
            fidl_transport_status.error().FormatDescription().c_str());
  }
}

}  // namespace display
