// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fuchsia/hardware/ethernet/c/banjo.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <limits.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/designware/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/ethernet/board/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <fbl/algorithm.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "vim3-gpios.h"
#include "vim3.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Irq> eth_mac_irqs{
    {{
        .irq = A311D_ETH_GMAC_IRQ,
        .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    }},
};

static const std::vector<fpbus::Mmio> eth_board_mmios{
    {{
        .base = A311D_PERIPHERALS_BASE,
        .length = A311D_PERIPHERALS_LENGTH,
    }},
    {{
        .base = A311D_HIU_BASE,
        .length = A311D_HIU_LENGTH,
    }},
};

static const std::vector<fpbus::Mmio> eth_mac_mmios{
    {{
        .base = A311D_ETH_MAC_BASE,
        .length = A311D_ETH_MAC_LENGTH,
    }},
};

static const std::vector<fpbus::Bti> eth_mac_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_ETHERNET,
    }},
};

static const std::vector<fpbus::BootMetadata> eth_mac_metadata{
    {{
        .zbi_type = DEVICE_METADATA_MAC_ADDRESS,
        .zbi_extra = 0,
    }},
};

static const fpbus::Node dwmac_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "dwmac";
  dev.vid() = PDEV_VID_DESIGNWARE;
  dev.did() = PDEV_DID_DESIGNWARE_ETH_MAC;
  dev.mmio() = eth_mac_mmios;
  dev.irq() = eth_mac_irqs;
  dev.bti() = eth_mac_btis;
  dev.boot_metadata() = eth_mac_metadata;
  return dev;
}();

const std::vector<fdf::BindRule> kEthBoardRules = {
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_ethernet_board::SERVICE,
                            bind_fuchsia_hardware_ethernet_board::SERVICE_ZIRCONTRANSPORT),
};

const std::vector<fdf::NodeProperty> kEthBoardProperties = {
    fdf::MakeProperty(bind_fuchsia_hardware_ethernet_board::SERVICE,
                      bind_fuchsia_hardware_ethernet_board::SERVICE_ZIRCONTRANSPORT),
};

const std::vector<fdf::ParentSpec> kEthBoardParents = {
    fdf::ParentSpec{{.bind_rules = kEthBoardRules, .properties = kEthBoardProperties}}};

zx_status_t AddEthComposite(fdf::WireSyncClient<fpbus::PlatformBus>& pbus,
                            fidl::AnyArena& fidl_arena, fdf::Arena& arena) {
  const std::vector<fdf::BindRule> kGpioIntRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                              bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, VIM3_GPIO_ID),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(VIM3_ETH_MAC_INTR)),
  };

  const std::vector<fdf::NodeProperty> kGpioIntProperties = std::vector{
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                        bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
  };

  const std::vector<fdf::BindRule> kGpioInitRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };
  const std::vector<fdf::NodeProperty> kGpioInitProperties = std::vector{
      fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };

  fpbus::Node eth_board_dev;
  eth_board_dev.name() = "ethernet_mac";
  eth_board_dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  eth_board_dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_A311D;
  eth_board_dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_ETH;
  eth_board_dev.mmio() = eth_board_mmios;

  const std::vector<fdf::ParentSpec> kEthParents = {
      fdf::ParentSpec{{
          .bind_rules = kGpioIntRules,
          .properties = kGpioIntProperties,
      }},
      fdf::ParentSpec{{
          .bind_rules = kGpioInitRules,
          .properties = kGpioInitProperties,
      }},
  };

  auto result = pbus.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, eth_board_dev),
      fidl::ToWire(fidl_arena,
                   fdf::CompositeNodeSpec{{.name = "ethernet_mac", .parents = kEthParents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Eth(eth_board_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Eth(eth_board_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

zx_status_t Vim3::EthInit() {
  // setup pinmux for RGMII connections
  gpio_init_steps_.push_back({A311D_GPIOZ(0), GpioSetAltFunction(A311D_GPIOZ_0_ETH_MDIO_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(1), GpioSetAltFunction(A311D_GPIOZ_1_ETH_MDC_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(2), GpioSetAltFunction(A311D_GPIOZ_2_ETH_RX_CLK_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(3), GpioSetAltFunction(A311D_GPIOZ_3_ETH_RX_DV_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(4), GpioSetAltFunction(A311D_GPIOZ_4_ETH_RXD0_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(5), GpioSetAltFunction(A311D_GPIOZ_5_ETH_RXD1_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(6), GpioSetAltFunction(A311D_GPIOZ_6_ETH_RXD2_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(7), GpioSetAltFunction(A311D_GPIOZ_7_ETH_RXD3_FN)});

  gpio_init_steps_.push_back({A311D_GPIOZ(8), GpioSetAltFunction(A311D_GPIOZ_8_ETH_TX_CLK_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(9), GpioSetAltFunction(A311D_GPIOZ_9_ETH_TX_EN_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(10), GpioSetAltFunction(A311D_GPIOZ_10_ETH_TXD0_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(11), GpioSetAltFunction(A311D_GPIOZ_11_ETH_TXD1_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(12), GpioSetAltFunction(A311D_GPIOZ_12_ETH_TXD2_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(13), GpioSetAltFunction(A311D_GPIOZ_13_ETH_TXD3_FN)});

  gpio_init_steps_.push_back({A311D_GPIOZ(0), GpioSetDriveStrength(2500)});
  gpio_init_steps_.push_back({A311D_GPIOZ(1), GpioSetDriveStrength(2500)});
  gpio_init_steps_.push_back({A311D_GPIOZ(2), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(3), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(4), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(5), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(6), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(7), GpioSetDriveStrength(3000)});

  gpio_init_steps_.push_back({A311D_GPIOZ(8), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(9), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(10), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(11), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(12), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(13), GpioSetDriveStrength(3000)});

  // Add a composite device for ethernet board in a new devhost.
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('ETH_');
  auto status = AddEthComposite(pbus_, fidl_arena, arena);
  if (status != ZX_OK) {
    return status;
  }

  // Add a composite device for dwmac driver in the ethernet board driver's driver host.
  auto spec = fdf::CompositeNodeSpec{{.name = "dwmac", .parents = kEthBoardParents}};
  fdf::WireUnownedResult dwmac_result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(arena, dwmac_dev), fidl::ToWire(arena, spec));
  if (!dwmac_result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Eth(dwmac_dev) request failed: %s", __func__,
           dwmac_result.FormatDescription().data());
    return dwmac_result.status();
  }
  if (dwmac_result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Eth(dwmac_dev) failed: %s", __func__,
           zx_status_get_string(dwmac_result->error_value()));
    return dwmac_result->error_value();
  }

  return ZX_OK;
}
}  // namespace vim3
