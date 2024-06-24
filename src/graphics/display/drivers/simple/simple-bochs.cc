// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.pci/cpp/wire.h>
#include <lib/device-protocol/pci.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/mmio/mmio-buffer.h>
#include <zircon/errors.h>
#include <zircon/process.h>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/simple/simple-display-driver.h"
#include "src/graphics/display/drivers/simple/simple-display.h"

namespace simple_display {

namespace {

constexpr int kDisplayWidth = 1024;
constexpr int kDisplayHeight = 768;
constexpr auto kDisplayFormat = fuchsia_images2::wire::PixelFormat::kB8G8R8A8;
constexpr int kBitsPerPixel = 32;

inline uint16_t bochs_vbe_dispi_read(MMIO_PTR void* base, uint32_t reg) {
  return MmioRead16(reinterpret_cast<MMIO_PTR uint16_t*>(reinterpret_cast<MMIO_PTR uint8_t*>(base) +
                                                         (0x500 + (reg << 1))));
}

inline void bochs_vbe_dispi_write(MMIO_PTR void* base, uint32_t reg, uint16_t val) {
  MmioWrite16(val, reinterpret_cast<MMIO_PTR uint16_t*>(reinterpret_cast<MMIO_PTR uint8_t*>(base) +
                                                        (0x500 + (reg << 1))));
}

#define BOCHS_VBE_DISPI_ID 0x0
#define BOCHS_VBE_DISPI_XRES 0x1
#define BOCHS_VBE_DISPI_YRES 0x2
#define BOCHS_VBE_DISPI_BPP 0x3
#define BOCHS_VBE_DISPI_ENABLE 0x4
#define BOCHS_VBE_DISPI_BANK 0x5
#define BOCHS_VBE_DISPI_VIRT_WIDTH 0x6
#define BOCHS_VBE_DISPI_VIRT_HEIGHT 0x7
#define BOCHS_VBE_DISPI_X_OFFSET 0x8
#define BOCHS_VBE_DISPI_Y_OFFSET 0x9
#define BOCHS_VBE_DISPI_VIDEO_MEMORY_64K 0xa

#define BOCHS_VBE_DISPI_ENABLED 0x01
#define BOCHS_VBE_DISPI_LFB_ENABLED 0x40

void set_hw_mode(MMIO_PTR void* regs, uint16_t width, uint16_t height, uint16_t bits_per_pixel) {
  FDF_LOG(TRACE, "id: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_ID));

  bochs_vbe_dispi_write(regs, BOCHS_VBE_DISPI_ENABLE, 0);
  bochs_vbe_dispi_write(regs, BOCHS_VBE_DISPI_BPP, bits_per_pixel);
  bochs_vbe_dispi_write(regs, BOCHS_VBE_DISPI_XRES, width);
  bochs_vbe_dispi_write(regs, BOCHS_VBE_DISPI_YRES, height);
  bochs_vbe_dispi_write(regs, BOCHS_VBE_DISPI_BANK, 0);
  bochs_vbe_dispi_write(regs, BOCHS_VBE_DISPI_VIRT_WIDTH, width);
  bochs_vbe_dispi_write(regs, BOCHS_VBE_DISPI_VIRT_HEIGHT, height);
  bochs_vbe_dispi_write(regs, BOCHS_VBE_DISPI_X_OFFSET, 0);
  bochs_vbe_dispi_write(regs, BOCHS_VBE_DISPI_Y_OFFSET, 0);
  bochs_vbe_dispi_write(regs, BOCHS_VBE_DISPI_ENABLE,
                        BOCHS_VBE_DISPI_ENABLED | BOCHS_VBE_DISPI_LFB_ENABLED);

  FDF_LOG(TRACE, "bochs_vbe_set_hw_mode:");
  FDF_LOG(TRACE, "     ID: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_ID));
  FDF_LOG(TRACE, "   XRES: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_XRES));
  FDF_LOG(TRACE, "   YRES: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_YRES));
  FDF_LOG(TRACE, "    BPP: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_BPP));
  FDF_LOG(TRACE, " ENABLE: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_ENABLE));
  FDF_LOG(TRACE, "   BANK: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_BANK));
  FDF_LOG(TRACE, "VWIDTH: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_VIRT_WIDTH));
  FDF_LOG(TRACE, "VHEIGHT: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_VIRT_HEIGHT));
  FDF_LOG(TRACE, "   XOFF: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_X_OFFSET));
  FDF_LOG(TRACE, "   YOFF: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_Y_OFFSET));
  FDF_LOG(TRACE, "    64K: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_VIDEO_MEMORY_64K));
}

class SimpleBochsDisplayDriver final : public SimpleDisplayDriver {
 public:
  explicit SimpleBochsDisplayDriver(fdf::DriverStartArgs start_args,
                                    fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  SimpleBochsDisplayDriver(const SimpleBochsDisplayDriver&) = delete;
  SimpleBochsDisplayDriver(SimpleBochsDisplayDriver&&) = delete;
  SimpleBochsDisplayDriver& operator=(const SimpleBochsDisplayDriver&) = delete;
  SimpleBochsDisplayDriver& operator=(SimpleBochsDisplayDriver&&) = delete;

  ~SimpleBochsDisplayDriver() override;

  // SimpleDisplayDriver:
  zx::result<> ConfigureHardware() override;
  zx::result<fdf::MmioBuffer> GetFrameBufferMmioBuffer() override;
  zx::result<DisplayProperties> GetDisplayProperties() override;

 private:
  zx::result<ddk::Pci> GetPciClient();
};

SimpleBochsDisplayDriver::SimpleBochsDisplayDriver(
    fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : SimpleDisplayDriver("bochs-vbe", std::move(start_args), std::move(driver_dispatcher)) {}

SimpleBochsDisplayDriver::~SimpleBochsDisplayDriver() = default;

zx::result<ddk::Pci> SimpleBochsDisplayDriver::GetPciClient() {
  zx::result<fidl::ClientEnd<fuchsia_hardware_pci::Device>> pci_result =
      incoming()->Connect<fuchsia_hardware_pci::Service::Device>("pci");
  if (pci_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to PCI protocol: %s", pci_result.status_string());
    return pci_result.take_error();
  }
  ddk::Pci pci(std::move(pci_result).value());
  ZX_DEBUG_ASSERT(pci.is_valid());
  return zx::ok(std::move(pci));
}

zx::result<> SimpleBochsDisplayDriver::ConfigureHardware() {
  zx::result<ddk::Pci> pci_result = GetPciClient();
  if (pci_result.is_error()) {
    return pci_result.take_error();
  }
  ddk::Pci pci = std::move(pci_result).value();

  std::optional<fdf::MmioBuffer> mmio;
  // map register window
  static constexpr uint32_t kQemuBochsRegisterMmioBarIndex = 2;
  zx_status_t status =
      pci.MapMmio(kQemuBochsRegisterMmioBarIndex, ZX_CACHE_POLICY_UNCACHED_DEVICE, &mmio);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to map PCI config: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  set_hw_mode(mmio->get(), kDisplayWidth, kDisplayHeight, kBitsPerPixel);
  return zx::ok();
}

zx::result<fdf::MmioBuffer> SimpleBochsDisplayDriver::GetFrameBufferMmioBuffer() {
  zx::result<ddk::Pci> pci_result = GetPciClient();
  if (pci_result.is_error()) {
    return pci_result.take_error();
  }
  ddk::Pci pci = std::move(pci_result).value();

  std::optional<fdf::MmioBuffer> framebuffer_mmio;
  static constexpr uint32_t kFramebufferPciBarIndex = 0;
  zx_status_t status =
      pci.MapMmio(kFramebufferPciBarIndex, ZX_CACHE_POLICY_WRITE_COMBINING, &framebuffer_mmio);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to map PCI bar %" PRIu32 ": %s", kFramebufferPciBarIndex,
            zx_status_get_string(status));
    return zx::error(status);
  }

  ZX_DEBUG_ASSERT(framebuffer_mmio.has_value());
  return zx::ok(std::move(framebuffer_mmio).value());
}

zx::result<DisplayProperties> SimpleBochsDisplayDriver::GetDisplayProperties() {
  static constexpr DisplayProperties kDisplayProperties = {
      .width_px = kDisplayWidth,
      .height_px = kDisplayHeight,
      .row_stride_px = kDisplayWidth,
      .pixel_format = kDisplayFormat,
  };
  return zx::ok(kDisplayProperties);
}

}  // namespace

}  // namespace simple_display

FUCHSIA_DRIVER_EXPORT(simple_display::SimpleBochsDisplayDriver);
