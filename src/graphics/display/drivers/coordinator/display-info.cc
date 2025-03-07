// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/display-info.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fit/result.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <zircon/assert.h>
#include <zircon/device/audio.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/time.h>

#include <cinttypes>
#include <cstddef>
#include <cstring>
#include <optional>
#include <span>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/string_printf.h>

#include "src/graphics/display/drivers/coordinator/added-display-info.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/edid/edid.h"

namespace display_coordinator {

// TODO(https://fxbug.dev/348695412): The two constructors below should be
// unified. The unified constructor should take in both `preferred_modes` and
// EDID bytes, and merge the modes.

DisplayInfo::DisplayInfo(display::DisplayId display_id,
                         fbl::Vector<display::PixelFormat> pixel_formats,
                         display::DisplayTiming preferred_mode)
    : IdMappable(display_id),
      edid(std::nullopt),
      mode(preferred_mode),
      pixel_formats(std::move(pixel_formats)) {
  ZX_DEBUG_ASSERT(display_id != display::kInvalidDisplayId);
}

DisplayInfo::DisplayInfo(display::DisplayId display_id,
                         fbl::Vector<display::PixelFormat> pixel_formats, edid::Edid edid)
    : IdMappable(display_id),
      edid(DisplayInfo::Edid{.base = std::move(edid)}),
      mode(std::nullopt),
      pixel_formats(std::move(pixel_formats)) {
  ZX_DEBUG_ASSERT(display_id != display::kInvalidDisplayId);

  // TODO(https://fxbug.dev/343872853): Parse audio information from EDID.
}

DisplayInfo::~DisplayInfo() = default;

void DisplayInfo::InitializeInspect(inspect::Node* parent_node) {
  node = parent_node->CreateChild(fbl::StringPrintf("display-%" PRIu64, id().value()).c_str());

  if (mode.has_value()) {
    node.CreateUint("width", mode->horizontal_active_px, &properties);
    node.CreateUint("height", mode->vertical_active_lines, &properties);
    return;
  }

  ZX_DEBUG_ASSERT(edid.has_value());

  node.CreateByteVector("edid-bytes", std::span(edid->base.edid_bytes(), edid->base.edid_length()),
                        &properties);

  size_t i = 0;
  for (const display::DisplayTiming& t : edid->timings) {
    auto child = node.CreateChild(fbl::StringPrintf("timing-parameters-%lu", ++i).c_str());
    child.CreateDouble("vsync-hz",
                       static_cast<double>(t.vertical_field_refresh_rate_millihertz()) / 1000.0,
                       &properties);
    child.CreateInt("pixel-clock-hz", t.pixel_clock_frequency_hz, &properties);
    child.CreateInt("horizontal-pixels", t.horizontal_active_px, &properties);
    child.CreateInt("horizontal-blanking", t.horizontal_blank_px(), &properties);
    child.CreateInt("horizontal-sync-offset", t.horizontal_front_porch_px, &properties);
    child.CreateInt("horizontal-sync-pulse", t.horizontal_sync_width_px, &properties);
    child.CreateInt("vertical-pixels", t.vertical_active_lines, &properties);
    child.CreateInt("vertical-blanking", t.vertical_blank_lines(), &properties);
    child.CreateInt("vertical-sync-offset", t.vertical_front_porch_lines, &properties);
    child.CreateInt("vertical-sync-pulse", t.vertical_sync_width_lines, &properties);
    properties.emplace(std::move(child));
  }
}

// static
zx::result<std::unique_ptr<DisplayInfo>> DisplayInfo::Create(AddedDisplayInfo added_display_info) {
  ZX_DEBUG_ASSERT(added_display_info.display_id != display::kInvalidDisplayId);
  display::DisplayId display_id = added_display_info.display_id;

  if (!added_display_info.banjo_preferred_modes.is_empty()) {
    ZX_DEBUG_ASSERT(added_display_info.banjo_preferred_modes.size() == 1);
    display::DisplayTiming preferred_mode =
        display::ToDisplayTiming(added_display_info.banjo_preferred_modes[0]);

    fbl::AllocChecker alloc_checker;
    auto display_info = fbl::make_unique_checked<DisplayInfo>(
        &alloc_checker, display_id, std::move(added_display_info.pixel_formats), preferred_mode);
    if (!alloc_checker.check()) {
      fdf::error("Failed to allocate DisplayInfo for display ID: {}", display_id.value());
      return zx::error(ZX_ERR_NO_MEMORY);
    }

    // TODO(https://fxbug.dev/348695412): This should not be an early return.
    // `preferred_modes` should be merged and de-duplicated with the modes
    // decoded from the display's EDID, by the logic below.
    return zx::ok(std::move(display_info));
  }

  if (added_display_info.edid_bytes.is_empty()) {
    fdf::error("Missing display timing information for display ID: {}", display_id.value());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fit::result<const char*, edid::Edid> edid_result =
      edid::Edid::Create(added_display_info.edid_bytes);
  if (edid_result.is_error()) {
    fdf::error("Failed to initialize EDID: {}", edid_result.error_value());
    return zx::error(ZX_ERR_INTERNAL);
  }

  fbl::AllocChecker alloc_checker;
  auto display_info = fbl::make_unique_checked<DisplayInfo>(
      &alloc_checker, display_id, std::move(added_display_info.pixel_formats),
      std::move(edid_result).value());
  if (!alloc_checker.check()) {
    fdf::error("Failed to allocate DisplayInfo for display ID: {}", display_id.value());
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_DEBUG) {
    const auto& edid = display_info->edid->base;
    std::string manufacturer_id = edid.GetManufacturerId();
    const char* manufacturer_name = edid.GetManufacturerName();
    const char* manufacturer =
        (strlen(manufacturer_name) > 0) ? manufacturer_name : manufacturer_id.c_str();

    std::string display_product_name = edid.GetDisplayProductName();
    std::string display_product_serial_number = edid.GetDisplayProductSerialNumber();

    fdf::debug("Manufacturer \"{}\", product {}, name \"{}\", serial \"{}\"", manufacturer,
               edid.product_code(), display_product_name, display_product_serial_number);
    edid.Print([](const char* str) { fdf::debug("{}", str); });
  }
  return zx::ok(std::move(display_info));
}

int DisplayInfo::GetHorizontalSizeMm() const {
  if (!edid.has_value()) {
    return 0;
  }
  return edid->base.horizontal_size_mm();
}

int DisplayInfo::GetVerticalSizeMm() const {
  if (!edid.has_value()) {
    return 0;
  }
  return edid->base.vertical_size_mm();
}

std::string_view DisplayInfo::GetManufacturerName() const {
  if (!edid.has_value()) {
    return std::string_view();
  }
  const char* manufacturer_name = edid->base.GetManufacturerName();
  return std::string_view(manufacturer_name);
}

std::string DisplayInfo::GetMonitorName() const {
  if (!edid.has_value()) {
    return {};
  }
  return edid->base.GetDisplayProductName();
}

std::string DisplayInfo::GetMonitorSerial() const {
  if (!edid.has_value()) {
    return {};
  }
  return edid->base.GetDisplayProductSerialNumber();
}

}  // namespace display_coordinator
