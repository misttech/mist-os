// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/image-format/image_format.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zx/result.h>

#include "src/graphics/display/drivers/simple/simple-display.h"

namespace simple_display {

namespace {

zx::result<DisplayProperties> GetDisplayPropertiesFromKernelFramebuffer(zx_device_t* device) {
  zbi_pixel_format_t format;
  uint32_t width, height, stride;
  zx_status_t status =
      zx_framebuffer_get_info(get_framebuffer_resource(device), &format, &width, &height, &stride);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get bootloader dimensions: %s", zx_status_get_string(status));
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  ZX_DEBUG_ASSERT(width <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(height <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(stride <= std::numeric_limits<int32_t>::max());

  fpromise::result<fuchsia_images2::wire::PixelFormat> sysmem2_format_type_result =
      ImageFormatConvertZbiToSysmemPixelFormat_v2(format);
  if (!sysmem2_format_type_result.is_ok()) {
    zxlogf(ERROR, "Failed to convert framebuffer format: %" PRIu32, static_cast<uint32_t>(format));
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  fuchsia_images2::wire::PixelFormat sysmem2_format = sysmem2_format_type_result.take_value();

  const DisplayProperties properties = {
      .width_px = static_cast<int32_t>(width),
      .height_px = static_cast<int32_t>(height),
      .row_stride_px = static_cast<int32_t>(stride),
      .pixel_format = sysmem2_format,
  };
  return zx::ok(properties);
}

zx_status_t BindIntelDisplay(void* ctx, zx_device_t* dev) {
  zx::result<DisplayProperties> get_display_properties_result =
      GetDisplayPropertiesFromKernelFramebuffer(dev);
  if (!get_display_properties_result.is_ok()) {
    zxlogf(ERROR, "Failed to get display properties from kernel framebuffer: %s",
           get_display_properties_result.status_string());
    return get_display_properties_result.error_value();
  }
  const DisplayProperties properties = std::move(get_display_properties_result).value();
  return bind_simple_pci_display(dev, "intel", /*bar=*/2u, properties);
}

constexpr zx_driver_ops_t kIntelDisplayDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = BindIntelDisplay,
};

}  // namespace

}  // namespace simple_display

ZIRCON_DRIVER(intel_disp, simple_display::kIntelDisplayDriverOps, "zircon", "0.1");
