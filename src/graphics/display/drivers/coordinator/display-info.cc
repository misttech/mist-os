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

DisplayInfo::DisplayInfo(display::DisplayId display_id,
                         fbl::Vector<display::PixelFormat> pixel_formats,
                         fbl::Vector<display::DisplayTiming> preferred_modes,
                         std::optional<edid::Edid> edid_info)
    : IdMappable(display_id),
      edid_info(std::move(edid_info)),
      timings(std::move(preferred_modes)),
      pixel_formats(std::move(pixel_formats)) {
  ZX_DEBUG_ASSERT(display_id != display::kInvalidDisplayId);

  // TODO(https://fxbug.dev/343872853): Parse audio information from EDID.
}

DisplayInfo::~DisplayInfo() = default;

void DisplayInfo::InitializeInspect(inspect::Node* parent_node) {
  node = parent_node->CreateChild(fbl::StringPrintf("display-%" PRIu64, id().value()).c_str());

  size_t i = 0;
  for (const display::DisplayTiming& t : timings) {
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

  if (!edid_info.has_value()) {
    return;
  }

  node.CreateByteVector("edid-bytes", std::span(edid_info->edid_bytes(), edid_info->edid_length()),
                        &properties);
}

// static
zx::result<std::unique_ptr<DisplayInfo>> DisplayInfo::Create(AddedDisplayInfo added_display_info) {
  ZX_DEBUG_ASSERT(added_display_info.display_id != display::kInvalidDisplayId);
  display::DisplayId display_id = added_display_info.display_id;

  fbl::Vector<display::DisplayTiming> preferred_modes;
  if (!added_display_info.banjo_preferred_modes.is_empty()) {
    fbl::AllocChecker alloc_checker;
    preferred_modes.reserve(added_display_info.banjo_preferred_modes.size(), &alloc_checker);
    if (!alloc_checker.check()) {
      fdf::error("Failed to allocate DisplayTiming list for display ID: {}", display_id.value());
      return zx::error(ZX_ERR_NO_MEMORY);
    }
    for (const display_mode_t& banjo_preferred_mode : added_display_info.banjo_preferred_modes) {
      ZX_DEBUG_ASSERT_MSG(
          preferred_modes.size() < added_display_info.banjo_preferred_modes.size(),
          "The push_back() below was not supposed to allocate memory, but it might");
      preferred_modes.push_back(display::ToDisplayTiming(banjo_preferred_mode), &alloc_checker);
      ZX_DEBUG_ASSERT_MSG(alloc_checker.check(),
                          "The push_back() above failed to allocate memory; "
                          "it was not supposed to allocate at all");
    }
  }

  std::optional<edid::Edid> edid_info;
  if (!added_display_info.edid_bytes.is_empty()) {
    fit::result<const char*, edid::Edid> edid_result =
        edid::Edid::Create(added_display_info.edid_bytes);
    if (edid_result.is_error()) {
      fdf::warn("Failed to initialize EDID: {}. Skipping EDID on display creation.",
                edid_result.error_value());
    } else {
      edid_info.emplace(std::move(edid_result).value());
    }
  }

  if (preferred_modes.is_empty() && !edid_info.has_value()) {
    fdf::error(
        "Failed to create DisplayInfo: The display doesn't provide "
        "a valid EDID nor valid preferred modes.");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fbl::AllocChecker alloc_checker;
  auto display_info = fbl::make_unique_checked<DisplayInfo>(
      &alloc_checker, display_id, std::move(added_display_info.pixel_formats),
      std::move(preferred_modes), std::move(edid_info));
  if (!alloc_checker.check()) {
    fdf::error("Failed to allocate DisplayInfo for display ID: {}", display_id.value());
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_DEBUG &&
      display_info->edid_info.has_value()) {
    const edid::Edid& edid = display_info->edid_info.value();
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
  if (!edid_info.has_value()) {
    return 0;
  }
  return edid_info->horizontal_size_mm();
}

int DisplayInfo::GetVerticalSizeMm() const {
  if (!edid_info.has_value()) {
    return 0;
  }
  return edid_info->vertical_size_mm();
}

std::string_view DisplayInfo::GetManufacturerName() const {
  if (!edid_info.has_value()) {
    return std::string_view();
  }
  const char* manufacturer_name = edid_info->GetManufacturerName();
  return std::string_view(manufacturer_name);
}

std::string DisplayInfo::GetMonitorName() const {
  if (!edid_info.has_value()) {
    return {};
  }
  return edid_info->GetDisplayProductName();
}

std::string DisplayInfo::GetMonitorSerial() const {
  if (!edid_info.has_value()) {
    return {};
  }
  return edid_info->GetDisplayProductSerialNumber();
}

}  // namespace display_coordinator
