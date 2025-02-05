// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <lib/device-protocol/pci.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/image-format/image_format.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zx/result.h>

#include <fbl/alloc_checker.h>

#include "lib/zbi-format/zbi.h"
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
  zx::result<zbi_swfb_t> GetFramebufferInfo();
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

zx::result<zbi_swfb_t> FramebufferIntelDisplayDriver::GetFramebufferInfo() {
  zx::result boot_items_client = incoming()->Connect<fuchsia_boot::Items>();
  if (boot_items_client.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to fuchsia.boot/Items: %s",
            boot_items_client.status_string());
    return boot_items_client.take_error();
  }
  fidl::WireResult result = fidl::WireCall(*boot_items_client)->Get2(ZBI_TYPE_FRAMEBUFFER, {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to call fuchsia.boot/Items.Get2: %s", result.status_string());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to get framebuffer boot item: %s",
            zx_status_get_string(result->error_value()));
    return zx::error(result->error_value());
  }
  fidl::VectorView items = result->value()->retrieved_items;
  if (items.count() == 0) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  if (items[0].length < sizeof(zbi_swfb_t)) {
    return zx::error(ZX_ERR_BAD_STATE);
  }
  zbi_swfb_t framebuffer_info;
  zx_status_t status = items[0].payload.read(&framebuffer_info, 0, sizeof(framebuffer_info));
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(framebuffer_info);
}

zx::result<DisplayProperties> FramebufferIntelDisplayDriver::GetDisplayProperties() {
  zx::result framebuffer_info = GetFramebufferInfo();
  if (framebuffer_info.is_error()) {
    FDF_LOG(ERROR, "Failed to get bootloader dimensions: %s", framebuffer_info.status_string());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  zbi_pixel_format_t format = framebuffer_info->format;
  uint32_t width = framebuffer_info->width;
  uint32_t height = framebuffer_info->height;
  uint32_t stride = framebuffer_info->stride;

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
