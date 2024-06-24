// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/device-protocol/pci.h>
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
  zxlogf(TRACE, "id: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_ID));

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

  zxlogf(TRACE, "bochs_vbe_set_hw_mode:");
  zxlogf(TRACE, "     ID: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_ID));
  zxlogf(TRACE, "   XRES: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_XRES));
  zxlogf(TRACE, "   YRES: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_YRES));
  zxlogf(TRACE, "    BPP: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_BPP));
  zxlogf(TRACE, " ENABLE: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_ENABLE));
  zxlogf(TRACE, "   BANK: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_BANK));
  zxlogf(TRACE, "VWIDTH: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_VIRT_WIDTH));
  zxlogf(TRACE, "VHEIGHT: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_VIRT_HEIGHT));
  zxlogf(TRACE, "   XOFF: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_X_OFFSET));
  zxlogf(TRACE, "   YOFF: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_Y_OFFSET));
  zxlogf(TRACE, "    64K: 0x%x", bochs_vbe_dispi_read(regs, BOCHS_VBE_DISPI_VIDEO_MEMORY_64K));
}

class SimpleBochsDisplayDriver final : public SimpleDisplayDriver {
 public:
  static zx::result<> Create(zx_device_t* parent);

  explicit SimpleBochsDisplayDriver(zx_device_t* parent);

  SimpleBochsDisplayDriver(const SimpleBochsDisplayDriver&) = delete;
  SimpleBochsDisplayDriver(SimpleBochsDisplayDriver&&) = delete;
  SimpleBochsDisplayDriver& operator=(const SimpleBochsDisplayDriver&) = delete;
  SimpleBochsDisplayDriver& operator=(SimpleBochsDisplayDriver&&) = delete;

  ~SimpleBochsDisplayDriver() override;

  // SimpleDisplayDriver:
  zx::result<> ConfigureHardware() override;
  zx::result<fdf::MmioBuffer> GetFrameBufferMmioBuffer() override;
  zx::result<DisplayProperties> GetDisplayProperties() override;
};

zx::result<> SimpleBochsDisplayDriver::Create(zx_device_t* parent) {
  fbl::AllocChecker alloc_checker;
  auto simple_bochs_display_driver =
      fbl::make_unique_checked<SimpleBochsDisplayDriver>(&alloc_checker, parent);
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for SimpleBochsDisplayDriver");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> init_result = simple_bochs_display_driver->Initialize();
  if (init_result.is_error()) {
    zxlogf(ERROR, "Failed to initialize SimpleBochsDisplayDriver: %s", init_result.status_string());
    return init_result.take_error();
  }

  // `simple_bochs_display_driver` is now managed by the driver manager.
  [[maybe_unused]] SimpleBochsDisplayDriver* driver_released =
      simple_bochs_display_driver.release();

  return zx::ok();
}

SimpleBochsDisplayDriver::SimpleBochsDisplayDriver(zx_device_t* parent)
    : SimpleDisplayDriver(parent, "bochs-vbe") {}

SimpleBochsDisplayDriver::~SimpleBochsDisplayDriver() = default;

zx::result<> SimpleBochsDisplayDriver::ConfigureHardware() {
  ddk::Pci pci(parent(), "pci");
  if (!pci.is_valid()) {
    zxlogf(ERROR, "Failed to get pci protocol");
    return zx::error(ZX_ERR_INTERNAL);
  }

  std::optional<fdf::MmioBuffer> mmio;
  // map register window
  static constexpr uint32_t kQemuBochsRegisterMmioBarIndex = 2;
  zx_status_t status =
      pci.MapMmio(kQemuBochsRegisterMmioBarIndex, ZX_CACHE_POLICY_UNCACHED_DEVICE, &mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to map pci config: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  set_hw_mode(mmio->get(), kDisplayWidth, kDisplayHeight, kBitsPerPixel);
  return zx::ok();
}

zx::result<fdf::MmioBuffer> SimpleBochsDisplayDriver::GetFrameBufferMmioBuffer() {
  ddk::Pci pci(parent(), "pci");
  if (!pci.is_valid()) {
    zxlogf(ERROR, "Failed to get PCI protocol");
    return zx::error(ZX_ERR_INTERNAL);
  }

  std::optional<fdf::MmioBuffer> framebuffer_mmio;
  static constexpr uint32_t kFramebufferPciBarIndex = 0;
  zx_status_t status =
      pci.MapMmio(kFramebufferPciBarIndex, ZX_CACHE_POLICY_WRITE_COMBINING, &framebuffer_mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to map pci bar %" PRIu32 ": %s", kFramebufferPciBarIndex,
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

constexpr zx_driver_ops_t kBochsVesaBiosExtensionDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind =
        [](void* ctx, zx_device_t* parent) {
          return SimpleBochsDisplayDriver::Create(parent).status_value();
        },
};

}  // namespace

}  // namespace simple_display

ZIRCON_DRIVER(bochs_vbe, simple_display::kBochsVesaBiosExtensionDriverOps, "zircon", "0.1");
