// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/display-info.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <zircon/assert.h>
#include <zircon/device/audio.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/time.h>

#include <cinttypes>
#include <cstdint>
#include <cstring>
#include <iterator>
#include <limits>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <fbl/string_printf.h>
#include <pretty/hexdump.h>

#include "src/graphics/display/drivers/coordinator/migration-util.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/edid/edid.h"

namespace display {

namespace {

edid::ddc_i2c_transact ddc_tx = [](void* ctx, edid::ddc_i2c_msg_t* msgs, uint32_t count) -> bool {
  auto i2c = static_cast<ddk::I2cImplProtocolClient*>(ctx);
  i2c_impl_op_t ops[count];
  for (unsigned i = 0; i < count; i++) {
    ops[i].address = msgs[i].addr;
    ops[i].data_buffer = msgs[i].buf;
    ops[i].data_size = msgs[i].length;
    ops[i].is_read = msgs[i].is_read;
    ops[i].stop = i == (count - 1);
  }
  return i2c->Transact(ops, count) == ZX_OK;
};

fit::result<const char*, DisplayInfo::Edid> InitEdidFromI2c(ddk::I2cImplProtocolClient& i2c) {
  const char* last_error = nullptr;

  static constexpr int kMaxEdidAttempts = 3;
  for (int attempt = 1; attempt <= kMaxEdidAttempts; attempt++) {
    fit::result<const char*, edid::Edid> result = edid::Edid::Create(&i2c, ddc_tx);
    if (result.is_ok()) {
      DisplayInfo::Edid edid = {
          .base = std::move(result).value(),
      };
      return fit::ok(std::move(edid));
    }
    last_error = result.error_value();
    FDF_LOG(WARNING, "Error %d/%d initializing edid: \"%s\"", attempt, kMaxEdidAttempts,
            last_error);
    zx::nanosleep(zx::deadline_after(zx::msec(5)));
  }

  ZX_DEBUG_ASSERT(last_error != nullptr);
  return fit::error(last_error);
}

fit::result<const char*, DisplayInfo::Edid> InitEdidFromBytes(cpp20::span<const uint8_t> bytes) {
  ZX_DEBUG_ASSERT(bytes.size() <= std::numeric_limits<uint16_t>::max());

  fit::result<const char*, edid::Edid> result = edid::Edid::Create(bytes);
  if (result.is_ok()) {
    DisplayInfo::Edid edid = {
        .base = std::move(result).value(),
    };
    return fit::ok(std::move(edid));
  }
  return result.take_error();
}

}  // namespace

DisplayInfo::DisplayInfo() = default;
DisplayInfo::~DisplayInfo() = default;

void DisplayInfo::InitializeInspect(inspect::Node* parent_node) {
  ZX_DEBUG_ASSERT(init_done);
  node = parent_node->CreateChild(fbl::StringPrintf("display-%" PRIu64, id.value()).c_str());

  if (mode.has_value()) {
    node.CreateUint("width", mode->horizontal_active_px, &properties);
    node.CreateUint("height", mode->vertical_active_lines, &properties);
    return;
  }

  ZX_DEBUG_ASSERT(edid.has_value());

  node.CreateByteVector(
      "edid-bytes", cpp20::span(edid->base.edid_bytes(), edid->base.edid_length()), &properties);

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
zx::result<fbl::RefPtr<DisplayInfo>> DisplayInfo::Create(
    const raw_display_info_t& banjo_display_info) {
  fbl::AllocChecker ac;
  fbl::RefPtr<DisplayInfo> out = fbl::AdoptRef(new (&ac) DisplayInfo);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  out->pending_layer_change = false;
  out->layer_count = 0;
  out->id = ToDisplayId(banjo_display_info.display_id);

  zx::result get_display_info_pixel_formats_result =
      CoordinatorPixelFormat::CreateFblVectorFromBanjoVector(cpp20::span(
          banjo_display_info.pixel_formats_list, banjo_display_info.pixel_formats_count));
  if (get_display_info_pixel_formats_result.is_error()) {
    FDF_LOG(ERROR, "Cannot convert pixel formats to FIDL pixel format value: %s",
            get_display_info_pixel_formats_result.status_string());
    return get_display_info_pixel_formats_result.take_error();
  }
  out->pixel_formats = std::move(get_display_info_pixel_formats_result.value());

  if (banjo_display_info.preferred_modes_count != 0) {
    ZX_DEBUG_ASSERT(banjo_display_info.preferred_modes_count == 1);

    out->mode = ToDisplayTiming(banjo_display_info.preferred_modes_list[0]);

    // TODO(https://fxbug.dev/348695412): This should not be an early return.
    // `preferred_modes` should be merged and de-duplicated with the modes
    // decoded from the display's EDID, by the logic below.
    return zx::ok(std::move(out));
  }

  auto edid_result = [&]() -> fit::result<const char*, Edid> {
    if (banjo_display_info.edid_bytes_count != 0) {
      cpp20::span<const uint8_t> edid_bytes(banjo_display_info.edid_bytes_list,
                                            banjo_display_info.edid_bytes_count);
      // TODO(https://fxbug.dev/348695412): Merge and de-duplicate the modes in
      // `preferred_modes` from the logic above.
      return InitEdidFromBytes(edid_bytes);
    }

    ddk::I2cImplProtocolClient i2c(&banjo_display_info.eddc_client);
    if (i2c.is_valid()) {
      // TODO(https://fxbug.dev/348695412): Merge and de-duplicate the modes in
      // `preferred_modes` from the logic above.
      return InitEdidFromI2c(i2c);
    }

    return fit::error("Missing display hardware support information");
  }();

  if (!edid_result.is_ok()) {
    FDF_LOG(ERROR, "Failed to initialize EDID: %s", edid_result.error_value());
    return zx::error(ZX_ERR_INTERNAL);
  }
  out->edid = std::move(edid_result).value();

  // TODO(https://fxbug.dev/343872853): Parse audio information from EDID.

  if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_DEBUG) {
    const auto& edid = out->edid->base;
    std::string manufacturer_id = edid.GetManufacturerId();
    const char* manufacturer_name = edid.GetManufacturerName();
    const char* manufacturer =
        (strlen(manufacturer_name) > 0) ? manufacturer_name : manufacturer_id.c_str();

    std::string display_product_name = edid.GetDisplayProductName();
    std::string display_product_serial_number = edid.GetDisplayProductSerialNumber();

    FDF_LOG(DEBUG, "Manufacturer \"%s\", product %d, name \"%s\", serial \"%s\"", manufacturer,
            edid.product_code(), display_product_name.c_str(),
            display_product_serial_number.c_str());
    edid.Print([](const char* str) { FDF_LOG(DEBUG, "%s", str); });
  }
  return zx::ok(std::move(out));
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

}  // namespace display
