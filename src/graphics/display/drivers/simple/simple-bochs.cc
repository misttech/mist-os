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

#include "simple-display.h"

namespace {

constexpr int kDisplayWidth = 1024;
constexpr int kDisplayHeight = 768;
constexpr auto kDisplayFormat = fuchsia_images2::wire::PixelFormat::kBgra32;
constexpr int kBitsPerPixel = 32;

}  // namespace

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

static void set_hw_mode(MMIO_PTR void* regs, uint16_t width, uint16_t height,
                        uint16_t bits_per_pixel) {
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

static zx_status_t bochs_vbe_bind(void* ctx, zx_device_t* dev) {
  ddk::Pci pci(dev, "pci");
  if (!pci.is_valid()) {
    printf("bochs-vbe: failed to get pci protocol\n");
    return ZX_ERR_INTERNAL;
  }

  std::optional<fdf::MmioBuffer> mmio;
  // map register window
  zx_status_t status = pci.MapMmio(2u, ZX_CACHE_POLICY_UNCACHED_DEVICE, &mmio);
  if (status != ZX_OK) {
    printf("bochs-vbe: failed to map pci config: %d\n", status);
    return status;
  }

  set_hw_mode(mmio->get(), kDisplayWidth, kDisplayHeight, kBitsPerPixel);

  return bind_simple_pci_display(dev, "bochs_vbe", 0u, kDisplayWidth, kDisplayHeight,
                                 /*stride=*/kDisplayWidth, kDisplayFormat);
}

static zx_driver_ops_t bochs_vbe_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = bochs_vbe_bind,
};

ZIRCON_DRIVER(bochs_vbe, bochs_vbe_driver_ops, "zircon", "0.1");
