// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/mmio/mmio.h>
#include <zircon/status.h>

#include <cstring>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/registers/cpp/bind.h>
#include <bind/fuchsia/hardware/usb/phy/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <bind/fuchsia/register/cpp/bind.h>
#include <bind/fuchsia/usb/phy/cpp/bind.h>
#include <ddk/usb-peripheral-config.h>
#include <ddktl/device.h>
#include <soc/aml-common/aml-registers.h>
#include <soc/aml-common/aml-usb-phy.h>
#include <soc/aml-meson/g12b-clk.h>
#include <usb/cdc.h>
#include <usb/dwc2/metadata.h>
#include <usb/peripheral-config.h>
#include <usb/peripheral.h>

#include "src/devices/board/drivers/vim3/vim3-gpios.h"
#include "src/devices/board/drivers/vim3/vim3.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> usb_phy_mmios{
    {{
        .base = A311D_USBCTRL_BASE,
        .length = A311D_USBCTRL_LENGTH,
    }},
    {{
        .base = A311D_USBPHY20_BASE,
        .length = A311D_USBPHY20_LENGTH,
    }},
    {{
        .base = A311D_USBPHY21_BASE,
        .length = A311D_USBPHY21_LENGTH,
    }},
    {{
        .base = A311D_USB3_PCIE_PHY_BASE,
        .length = A311D_USB3_PCIE_PHY_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> usb_phy_irqs{
    {{
        .irq = A311D_USB_IDDIG_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
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
static const PhyType type = kG12B;

// aml_usb_phy manages 3 different controllers:
//  - One USB 2.0 controller that is only supports host mode.
//  - One USB 2.0 controller that supports OTG (both host and device mode).
//  - One USB 3.0 controller that only supports host mode.
// The two USB-A ports both are connected to the USB 2.0 host only controller. The USB-A port
// closest to the ethernet port is connected also the the USB 3.0 host only controller. The USB-C
// port is connected to the USB 2.0 OTG controller, however, we only want the USB-C port to be in
// peripheral mode to support USB-CDC use-case.
static const std::vector<UsbPhyMode> phy_modes = {
    {UsbProtocol::Usb2_0, UsbMode::Host, false},
    {UsbProtocol::Usb2_0, UsbMode::Peripheral, true},
    {UsbProtocol::Usb3_0, UsbMode::Host, false},
};

static const std::vector<fpbus::Metadata> usb_phy_metadata{
    {{
        .type = DEVICE_METADATA_PRIVATE,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&pll_settings),
            reinterpret_cast<const uint8_t*>(&pll_settings) + sizeof(pll_settings)),
    }},
    {{
        .type = DEVICE_METADATA_PRIVATE_PHY_TYPE | DEVICE_METADATA_PRIVATE,
        .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&type),
                                     reinterpret_cast<const uint8_t*>(&type) + sizeof(type)),
    }},
    {{
        .type = DEVICE_METADATA_USB_MODE,
        .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(phy_modes.data()),
                                     reinterpret_cast<const uint8_t*>(phy_modes.data()) +
                                         phy_modes.size() * sizeof(UsbPhyMode)),
    }},
};

static const fpbus::Node usb_phy_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "usb-phy-pdev";
  dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_USB_PHY_V2;
  dev.mmio() = usb_phy_mmios;
  dev.irq() = usb_phy_irqs;
  dev.bti() = usb_btis;
  dev.metadata() = usb_phy_metadata;
  return dev;
}();

const std::vector<fuchsia_driver_framework::BindRule> kResetRegisterRules = {
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_registers::SERVICE,
                            bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia_register::NAME,
                            bind_fuchsia_amlogic_platform::NAME_REGISTER_USB_PHY_V2_RESET)};

const std::vector<fuchsia_driver_framework::NodeProperty> kResetRegisterProperties = {
    fdf::MakeProperty(bind_fuchsia_hardware_registers::SERVICE,
                      bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_register::NAME,
                      bind_fuchsia_amlogic_platform::NAME_REGISTER_USB_PHY_V2_RESET)};

const std::vector<fdf::BindRule> kGpioInitRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

const std::vector<fdf::NodeProperty> kGpioInitProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

const std::vector<fdf::BindRule> kClockInitRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_clock::BIND_INIT_STEP_CLOCK),
};

const std::vector<fdf::NodeProperty> kClockInitProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_clock::BIND_INIT_STEP_CLOCK),
};

const std::vector<fuchsia_driver_framework::ParentSpec> kUsbPhyDevParents = {
    fuchsia_driver_framework::ParentSpec{{
        .bind_rules = kResetRegisterRules,
        .properties = kResetRegisterProperties,
    }},
    fuchsia_driver_framework::ParentSpec{{
        .bind_rules = kGpioInitRules,
        .properties = kGpioInitProperties,
    }},
    fuchsia_driver_framework::ParentSpec{{
        .bind_rules = kClockInitRules,
        .properties = kClockInitProperties,
    }},
};

static const std::vector<fpbus::Mmio> dwc2_mmios{
    {{
        .base = A311D_USB1_BASE,
        .length = A311D_USB1_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> dwc2_irqs{
    {{
        .irq = A311D_USB1_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Bti> dwc2_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_USB,
    }},
};

// Metadata for DWC2 driver.
static const dwc2_metadata_t dwc2_metadata = {
    .dma_burst_len = DWC2_DMA_BURST_INCR8,
    .usb_turnaround_time = 9,
    .rx_fifo_size = 256,   // for all OUT endpoints.
    .nptx_fifo_size = 32,  // for endpoint zero IN direction.
    .tx_fifo_sizes =
        {
            128,  // for CDC ethernet bulk IN.
            4,    // for CDC ethernet interrupt IN.
            128,  // for test function bulk IN.
            16,   // for test function interrupt IN.
        },
};

static const std::vector<fpbus::Mmio> xhci_mmios{
    {{
        .base = A311D_USB0_BASE,
        .length = A311D_USB0_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> xhci_irqs{
    {{
        .irq = A311D_USB0_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const fpbus::Node xhci_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "xhci-pdev";
  dev.vid() = PDEV_VID_GENERIC;
  dev.pid() = PDEV_PID_GENERIC;
  dev.did() = PDEV_DID_USB_XHCI;
  dev.mmio() = xhci_mmios;
  dev.irq() = xhci_irqs;
  dev.bti() = usb_btis;
  return dev;
}();

using FunctionDescriptor = fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor;

static const std::vector<fpbus::Metadata> usb_metadata{
    {{
        .type = DEVICE_METADATA_USB_CONFIG,
    }},
    {{
        .type = DEVICE_METADATA_PRIVATE,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&dwc2_metadata),
            reinterpret_cast<const uint8_t*>(&dwc2_metadata) + sizeof(dwc2_metadata)),
    }},
};

static const std::vector<fpbus::BootMetadata> usb_boot_metadata{
    {{
        .zbi_type = DEVICE_METADATA_MAC_ADDRESS,
        .zbi_extra = MACADDR_WIFI,
    }},
};

static fpbus::Node dwc2_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "dwc2-pdev";
  dev.vid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC;
  dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  dev.did() = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_USB_DWC2;
  dev.mmio() = dwc2_mmios;
  dev.irq() = dwc2_irqs;
  dev.bti() = dwc2_btis;
  dev.metadata() = usb_metadata;
  dev.boot_metadata() = usb_boot_metadata;
  return dev;
}();

zx_status_t AddDwc2Composite(fdf::WireSyncClient<fpbus::PlatformBus>& pbus,
                             fidl::AnyArena& fidl_arena, fdf::Arena& arena) {
  const std::vector<fdf::BindRule> kDwc2PhyRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_usb_phy::SERVICE,
                              bind_fuchsia_hardware_usb_phy::SERVICE_DRIVERTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                              bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
      fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_PID,
                              bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
      fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID,
                              bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_USB_DWC2),
  };

  const std::vector<fdf::NodeProperty> kDwc2PhyProperties = std::vector{
      fdf::MakeProperty(bind_fuchsia_hardware_usb_phy::SERVICE,
                        bind_fuchsia_hardware_usb_phy::SERVICE_DRIVERTRANSPORT),
      fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                        bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
      fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_PID,
                        bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
      fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                        bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_USB_DWC2),
  };

  const std::vector<fdf::ParentSpec> kDwc2Parents{{kDwc2PhyRules, kDwc2PhyProperties}};
  auto dwc2_result = pbus.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, dwc2_dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "dwc2-composite", .parents = kDwc2Parents}}));
  if (!dwc2_result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Usb(dwc2_phy) request failed: %s",
           dwc2_result.FormatDescription().data());
    return dwc2_result.status();
  }
  if (dwc2_result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Usb(dwc2_phy) failed: %s",
           zx_status_get_string(dwc2_result->error_value()));
    return dwc2_result->error_value();
  }
  return ZX_OK;
}

zx_status_t AddXhciComposite(fdf::WireSyncClient<fpbus::PlatformBus>& pbus,
                             fidl::AnyArena& fidl_arena, fdf::Arena& arena) {
  const std::vector<fuchsia_driver_framework::BindRule> kXhciCompositeRules = {
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_usb_phy::SERVICE,
                              bind_fuchsia_hardware_usb_phy::SERVICE_DRIVERTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                              bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
      fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_PID,
                              bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
      fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID,
                              bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_XHCI),
  };
  const std::vector<fuchsia_driver_framework::NodeProperty> kXhciCompositeProperties = {
      fdf::MakeProperty(bind_fuchsia_hardware_usb_phy::SERVICE,
                        bind_fuchsia_hardware_usb_phy::SERVICE_DRIVERTRANSPORT),
      fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                        bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
      fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_PID,
                        bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
      fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                        bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_XHCI),
  };

  const std::vector<fuchsia_driver_framework::ParentSpec> kXhciParents = {
      fuchsia_driver_framework::ParentSpec{
          {.bind_rules = kXhciCompositeRules, .properties = kXhciCompositeProperties}}};
  auto result = pbus.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, xhci_dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "xhci-composite", .parents = kXhciParents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Usb(xhci-phy) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Usb(xhci-phy) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

zx_status_t Vim3::UsbInit() {
  using fuchsia_hardware_clockimpl::wire::InitCall;

  // Turn on clocks.
  clock_init_steps_.push_back({g12b_clk::G12B_CLK_USB, InitCall::WithEnable({})});

  clock_init_steps_.push_back({g12b_clk::G12B_CLK_USB1_TO_DDR, InitCall::WithEnable({})});

  clock_init_steps_.push_back({g12b_clk::CLK_PCIE_PLL, InitCall::WithDisable({})});

  clock_init_steps_.push_back(
      {g12b_clk::CLK_PCIE_PLL, InitCall::WithRateHz(init_arena_, 100'000'000)});

  clock_init_steps_.push_back({g12b_clk::CLK_PCIE_PLL, InitCall::WithEnable({})});

  // Power on USB.
  gpio_init_steps_.push_back({VIM3_USB_PWR, GpioConfigOut(1)});

  // Create USB Phy Device
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('USB_');
  auto spec = fuchsia_driver_framework::CompositeNodeSpec{
      {.name = "usb-phy-composite", .parents = kUsbPhyDevParents}};
  fdf::WireUnownedResult usb_phy_result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, usb_phy_dev), fidl::ToWire(fidl_arena, spec));
  if (!usb_phy_result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Usb(usb_phy_dev) request failed: %s",
           usb_phy_result.FormatDescription().data());
    return usb_phy_result.status();
  }
  if (usb_phy_result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Usb(usb_phy_dev) failed: %s",
           zx_status_get_string(usb_phy_result->error_value()));
    return usb_phy_result->error_value();
  }

  // Create DWC2 Device
  std::unique_ptr<usb::UsbPeripheralConfig> peripheral_config;
  auto status = usb::UsbPeripheralConfig::CreateFromBootArgs(parent_, &peripheral_config);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get usb config from boot args - %d", status);
    return status;
  }
  dwc2_dev.metadata().value()[0].data().emplace(peripheral_config->config_data());
  status = AddDwc2Composite(pbus_, fidl_arena, arena);
  if (status != ZX_OK) {
    return status;
  }

  // Create XHCI device.
  status = AddXhciComposite(pbus_, fidl_arena, arena);
  if (status != ZX_OK) {
    return status;
  }

  return ZX_OK;
}

}  // namespace vim3
