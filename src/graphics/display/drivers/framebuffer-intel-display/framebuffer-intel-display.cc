// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/device-protocol/pci.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/image-format/image_format.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zx/result.h>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"
#include "src/graphics/display/lib/framebuffer-display/framebuffer-display-driver.h"
#include "src/graphics/display/lib/framebuffer-display/framebuffer-display.h"

namespace framebuffer_display {

namespace {

class FramebufferIntelDisplayDriver final : public FramebufferDisplayDriver {
 public:
  explicit FramebufferIntelDisplayDriver(fdf::DriverStartArgs start_args,
                                         fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  FramebufferIntelDisplayDriver(const FramebufferIntelDisplayDriver&) = delete;
  FramebufferIntelDisplayDriver(FramebufferIntelDisplayDriver&&) = delete;
  FramebufferIntelDisplayDriver& operator=(const FramebufferIntelDisplayDriver&) = delete;
  FramebufferIntelDisplayDriver& operator=(FramebufferIntelDisplayDriver&&) = delete;

  ~FramebufferIntelDisplayDriver() override;

  // FramebufferDisplayDriver:
  zx::result<> ConfigureHardware() override;
  zx::result<fdf::MmioBuffer> GetFrameBufferMmioBuffer() override;
  zx::result<DisplayProperties> GetDisplayProperties() override;

 private:
  zx::result<zx::resource> GetFramebufferResource();
};

FramebufferIntelDisplayDriver::FramebufferIntelDisplayDriver(
    fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : FramebufferDisplayDriver("framebuffer-intel-display", std::move(start_args),
                               std::move(driver_dispatcher)) {}

FramebufferIntelDisplayDriver::~FramebufferIntelDisplayDriver() = default;

zx::result<> FramebufferIntelDisplayDriver::ConfigureHardware() { return zx::ok(); }

zx::result<fdf::MmioBuffer> FramebufferIntelDisplayDriver::GetFrameBufferMmioBuffer() {
  zx::result<fidl::ClientEnd<fuchsia_hardware_pci::Device>> pci_result =
      incoming()->Connect<fuchsia_hardware_pci::Service::Device>("pci");
  if (pci_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to PCI protocol: %s", pci_result.status_string());
    return pci_result.take_error();
  }
  ddk::Pci pci(std::move(pci_result).value());
  ZX_DEBUG_ASSERT(pci.is_valid());

  std::optional<fdf::MmioBuffer> framebuffer_mmio;
  static constexpr uint32_t kIntelFramebufferPciBarIndex = 2;
  zx_status_t status =
      pci.MapMmio(kIntelFramebufferPciBarIndex, ZX_CACHE_POLICY_WRITE_COMBINING, &framebuffer_mmio);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to map PCI bar %" PRIu32 ": %s", kIntelFramebufferPciBarIndex,
            zx_status_get_string(status));
    return zx::error(status);
  }

  ZX_DEBUG_ASSERT(framebuffer_mmio.has_value());
  return zx::ok(std::move(framebuffer_mmio).value());
}

zx::result<zx::resource> FramebufferIntelDisplayDriver::GetFramebufferResource() {
  zx::result framebuffer_resource_result =
      incoming()->Connect<fuchsia_kernel::FramebufferResource>();
  if (framebuffer_resource_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to framebuffer resource: %s",
            framebuffer_resource_result.status_string());
    return framebuffer_resource_result.take_error();
  }
  fidl::WireSyncClient<fuchsia_kernel::FramebufferResource> framebuffer_resource =
      fidl::WireSyncClient(std::move(framebuffer_resource_result).value());
  fidl::WireResult<fuchsia_kernel::FramebufferResource::Get> get_result =
      framebuffer_resource->Get();
  if (!get_result.ok()) {
    FDF_LOG(ERROR, "Failed to get framebuffer resource: %s", get_result.status_string());
    return zx::error(get_result.status());
  }
  return zx::ok(std::move(std::move(get_result).value()).resource);
}

zx::result<DisplayProperties> FramebufferIntelDisplayDriver::GetDisplayProperties() {
  zx::result<zx::resource> framebuffer_resource_result = GetFramebufferResource();
  if (framebuffer_resource_result.is_error()) {
    return framebuffer_resource_result.take_error();
  }
  zx::resource framebuffer_resource = std::move(framebuffer_resource_result).value();

  zbi_pixel_format_t format;
  uint32_t width, height, stride;
  zx_status_t status =
      zx_framebuffer_get_info(framebuffer_resource.get(), &format, &width, &height, &stride);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to get bootloader dimensions: %s", zx_status_get_string(status));
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  ZX_DEBUG_ASSERT(width <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(height <= std::numeric_limits<int32_t>::max());
  ZX_DEBUG_ASSERT(stride <= std::numeric_limits<int32_t>::max());

  fpromise::result<fuchsia_images2::wire::PixelFormat> sysmem2_format_type_result =
      ImageFormatConvertZbiToSysmemPixelFormat_v2(format);
  if (!sysmem2_format_type_result.is_ok()) {
    FDF_LOG(ERROR, "Failed to convert framebuffer format: %" PRIu32, static_cast<uint32_t>(format));
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  fuchsia_images2::wire::PixelFormat sysmem2_format = sysmem2_format_type_result.take_value();

  if (!display::PixelFormat::IsSupported(sysmem2_format)) {
    FDF_LOG(ERROR, "Unsupported framebuffer format: %" PRIu32,
            static_cast<uint32_t>(sysmem2_format));
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  display::PixelFormat pixel_format(sysmem2_format);

  const DisplayProperties properties = {
      .width_px = static_cast<int32_t>(width),
      .height_px = static_cast<int32_t>(height),
      .row_stride_px = static_cast<int32_t>(stride),
      .pixel_format = pixel_format,
  };
  return zx::ok(properties);
}

}  // namespace

}  // namespace framebuffer_display

FUCHSIA_DRIVER_EXPORT(framebuffer_display::FramebufferIntelDisplayDriver);
