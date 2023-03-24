// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/align.h>
#include <zircon/status.h>
#include <zircon/syscalls/smc.h>

#include <optional>

#include <ddk/usb-peripheral-config.h>
#include <soc/aml-a1/a1-gpio.h>
#include <soc/aml-a1/a1-hw.h>
#include <soc/aml-common/aml-registers.h>
#include <usb/cdc.h>
#include <usb/dwc2/metadata.h>
#include <usb/peripheral-config.h>
#include <usb/peripheral.h>
#include <usb/usb.h>

#include "src/devices/board/drivers/clover/clover.h"
#include "src/devices/board/drivers/clover/xhci-bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace clover {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> usb_phy_mmios{
    {{
        .base = A1_USBCTRL_BASE,
        .length = A1_USBCTRL_LENGTH,
    }},
    {{
        .base = A1_USBPHY_BASE,
        .length = A1_USBPHY_LENGTH,
    }},
    {{
        .base = A1_RESET_BASE,
        .length = A1_RESET_LENGTH,
    }},
    {{
        .base = A1_CLK_BASE,
        .length = A1_CLK_LENGTH,
    }},
};

static const std::vector<fpbus::Bti> usb_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_USB,
    }},
};

// Static PLL configuration parameters.
static const uint32_t pll_settings[] = {
    0x09400414, 0x927e0000, 0xac5f69e5, 0xfe18, 0x8000fff, 0x78000, 0xe0004, 0xe000c,
};

static const usb_mode_t dr_mode = USB_MODE_HOST;

static const std::vector<fpbus::Metadata> usb_phy_metadata{
    {{
        .type = DEVICE_METADATA_PRIVATE,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&pll_settings),
            reinterpret_cast<const uint8_t*>(&pll_settings) + sizeof(pll_settings)),
    }},
    {{
        .type = DEVICE_METADATA_USB_MODE,
        .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&dr_mode),
                                     reinterpret_cast<const uint8_t*>(&dr_mode) + sizeof(dr_mode)),
    }},
};

static const std::vector<fpbus::Smc> usb_smcs{
    {{
        .service_call_num_base = ARM_SMC_SERVICE_CALL_NUM_SIP_SERVICE_BASE,
        .count = ARM_SMC_SERVICE_CALL_NUM_SIP_SERVICE_LENGTH,
        .exclusive = false,
    }},
};

static const fpbus::Node usb_phy_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "a1-usb-phy";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_AMLOGIC_A1;
  dev.did() = PDEV_DID_AMLOGIC_A1_USB_PHY;
  dev.mmio() = usb_phy_mmios;
  dev.bti() = usb_btis;
  dev.smc() = usb_smcs;
  dev.metadata() = usb_phy_metadata;
  return dev;
}();

static const std::vector<fpbus::Mmio> xhci_mmios{
    {{
        .base = A1_USB_BASE,
        .length = A1_USB_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> xhci_irqs{
    {{
        .irq = A1_USB_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const fpbus::Node xhci_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "xhci";
  dev.vid() = PDEV_VID_GENERIC;
  dev.pid() = PDEV_PID_GENERIC;
  dev.did() = PDEV_DID_USB_XHCI_COMPOSITE;
  dev.mmio() = xhci_mmios;
  dev.irq() = xhci_irqs;
  dev.bti() = usb_btis;
  return dev;
}();

zx_status_t Clover::UsbInit() {
  // Create USB Phy Device
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('USB_');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, usb_phy_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd Usb(usb_phy_dev) request failed: %s", result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd Usb(usb_phy_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  // Create XHCI device.
  {
    auto result = pbus_.buffer(arena)->AddComposite(
        fidl::ToWire(fidl_arena, xhci_dev),
        platform_bus_composite::MakeFidlFragment(fidl_arena, xhci_fragments,
                                                 std::size(xhci_fragments)),
        "xhci-phy");
    if (!result.ok()) {
      zxlogf(ERROR, "AddComposite Usb(xhci_dev) request failed: %s",
             result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddComposite Usb(xhci_dev) failed: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  return ZX_OK;
}

}  // namespace clover
