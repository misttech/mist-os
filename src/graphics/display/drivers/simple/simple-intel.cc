// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/device-protocol/pci.h>
#include <lib/image-format/image_format.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zx/result.h>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/simple/simple-display-driver.h"
#include "src/graphics/display/drivers/simple/simple-display.h"

namespace simple_display {

namespace {

class SimpleIntelDisplayDriver final : public SimpleDisplayDriver {
 public:
  static zx::result<> Create(zx_device_t* parent);

  explicit SimpleIntelDisplayDriver(zx_device_t* parent);

  SimpleIntelDisplayDriver(const SimpleIntelDisplayDriver&) = delete;
  SimpleIntelDisplayDriver(SimpleIntelDisplayDriver&&) = delete;
  SimpleIntelDisplayDriver& operator=(const SimpleIntelDisplayDriver&) = delete;
  SimpleIntelDisplayDriver& operator=(SimpleIntelDisplayDriver&&) = delete;

  ~SimpleIntelDisplayDriver() override;

  // SimpleDisplayDriver:
  zx::result<> ConfigureHardware() override;
  zx::result<fdf::MmioBuffer> GetFrameBufferMmioBuffer() override;
  zx::result<DisplayProperties> GetDisplayProperties() override;
};

zx::result<> SimpleIntelDisplayDriver::Create(zx_device_t* parent) {
  fbl::AllocChecker alloc_checker;
  auto simple_intel_display_driver =
      fbl::make_unique_checked<SimpleIntelDisplayDriver>(&alloc_checker, parent);
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for SimpleIntelDisplayDriver");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> init_result = simple_intel_display_driver->Initialize();
  if (init_result.is_error()) {
    zxlogf(ERROR, "Failed to initialize SimpleIntelDisplayDriver: %s", init_result.status_string());
    return init_result.take_error();
  }

  // `simple_intel_display_driver` is now managed by the driver manager.
  [[maybe_unused]] SimpleIntelDisplayDriver* driver_released =
      simple_intel_display_driver.release();

  return zx::ok();
}

SimpleIntelDisplayDriver::SimpleIntelDisplayDriver(zx_device_t* parent)
    : SimpleDisplayDriver(parent, "intel") {}

SimpleIntelDisplayDriver::~SimpleIntelDisplayDriver() = default;

zx::result<> SimpleIntelDisplayDriver::ConfigureHardware() { return zx::ok(); }

zx::result<fdf::MmioBuffer> SimpleIntelDisplayDriver::GetFrameBufferMmioBuffer() {
  ddk::Pci pci(parent(), "pci");
  if (!pci.is_valid()) {
    zxlogf(ERROR, "Failed to get PCI protocol");
    return zx::error(ZX_ERR_INTERNAL);
  }

  std::optional<fdf::MmioBuffer> framebuffer_mmio;
  static constexpr uint32_t kIntelFramebufferPciBarIndex = 2;
  zx_status_t status =
      pci.MapMmio(kIntelFramebufferPciBarIndex, ZX_CACHE_POLICY_WRITE_COMBINING, &framebuffer_mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to map pci bar %" PRIu32 ": %s", kIntelFramebufferPciBarIndex,
           zx_status_get_string(status));
    return zx::error(status);
  }

  ZX_DEBUG_ASSERT(framebuffer_mmio.has_value());
  return zx::ok(std::move(framebuffer_mmio).value());
}

zx::result<DisplayProperties> SimpleIntelDisplayDriver::GetDisplayProperties() {
  zbi_pixel_format_t format;
  uint32_t width, height, stride;
  zx_status_t status = zx_framebuffer_get_info(get_framebuffer_resource(parent()), &format, &width,
                                               &height, &stride);
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

constexpr zx_driver_ops_t kIntelDisplayDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind =
        [](void* ctx, zx_device_t* parent) {
          return SimpleIntelDisplayDriver::Create(parent).status_value();
        },
};

}  // namespace

}  // namespace simple_display

ZIRCON_DRIVER(intel_disp, simple_display::kIntelDisplayDriverOps, "zircon", "0.1");
