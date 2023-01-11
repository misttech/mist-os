// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fuchsia/hardware/gpioimpl/cpp/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <fbl/algorithm.h>
#include <soc/as370/as370-gpio.h>
#include <soc/as370/as370-hw.h>

#include "pinecrest.h"
#include "src/devices/board/drivers/pinecrest/pinecrest-wifi-bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"
#include "src/devices/lib/nxp/include/wifi/wifi-config.h"

namespace board_pinecrest {
namespace fpbus = fuchsia_hardware_platform_bus;

zx_status_t Pinecrest::SdioInit() {
  zx_status_t status;

  static const std::vector<fpbus::Mmio> sdio_mmios{
      {{
          .base = as370::kSdio0Base,
          .length = as370::kSdio0Size,
      }},
  };

  static const std::vector<fpbus::Irq> sdio_irqs{
      {{
          .irq = as370::kSdio0Irq,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
  };

  static const std::vector<fpbus::Bti> sdio_btis{
      {{
          .iommu_index = 0,
          .bti_id = BTI_SDIO0,
      }},
  };

  static const wlan::nxpfmac::NxpSdioWifiConfig wifi_config = {
      .client_support = true,
      .softap_support = true,
      .sdio_rx_aggr_enable = true,
      .fixed_beacon_buffer = false,
      .auto_ds = true,
      .ps_mode = false,
      .max_tx_buf = 2048,
      .cfg_11d = false,
      .inact_tmo = false,
      .hs_wake_interval = 400,
      .indication_gpio = 0xff,
  };

  static const std::vector<fpbus::Metadata> wifi_metadata{
      {{
          .type = DEVICE_METADATA_WIFI_CONFIG,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&wifi_config),
              reinterpret_cast<const uint8_t*>(&wifi_config) + sizeof(wifi_config)),
      }},
  };

  static const std::vector<fpbus::BootMetadata> wifi_boot_metadata{
      {{
          .zbi_type = DEVICE_METADATA_MAC_ADDRESS,
          .zbi_extra = MACADDR_WIFI,
      }},
  };

  static const std::vector<fpbus::BootMetadata> sd_emmc_boot_metadata{
      {{
          .zbi_type = DEVICE_METADATA_MAC_ADDRESS,
          .zbi_extra = MACADDR_BLUETOOTH,
      }},
  };

  fpbus::Node sdio_dev;
  sdio_dev.name() = "pinecrest-sdio";
  sdio_dev.vid() = PDEV_VID_SYNAPTICS;
  sdio_dev.pid() = PDEV_PID_SYNAPTICS_AS370;
  sdio_dev.did() = PDEV_DID_AS370_SDHCI0;
  sdio_dev.irq() = sdio_irqs;
  sdio_dev.mmio() = sdio_mmios;
  sdio_dev.bti() = sdio_btis;
  sdio_dev.boot_metadata() = sd_emmc_boot_metadata;

  // Configure eMMC-SD soc pads.
  if (((status = gpio_impl_.SetAltFunction(58, 1)) != ZX_OK) ||  // SD0_CLK
      ((status = gpio_impl_.SetAltFunction(61, 1)) != ZX_OK) ||  // SD0_CMD
      ((status = gpio_impl_.SetAltFunction(56, 1)) != ZX_OK) ||  // SD0_DAT0
      ((status = gpio_impl_.SetAltFunction(57, 1)) != ZX_OK) ||  // SD0_DAT1
      ((status = gpio_impl_.SetAltFunction(59, 1)) != ZX_OK) ||  // SD0_DAT2
      ((status = gpio_impl_.SetAltFunction(60, 1)) != ZX_OK) ||  // SD0_DAT3
      ((status = gpio_impl_.SetAltFunction(62, 1)) != ZX_OK) ||  // SD0_CDn
      ((status = gpio_impl_.SetAltFunction(63, 0)) != ZX_OK)) {  // SDIO_PWR_EN | WLAN_EN
    return status;
  }

  status = gpio_impl_.ConfigOut(63, 1);  // Disable WLAN Powerdown
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: SDIO Power/WLAN Enable error: %d", __func__, status);
  }

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('SDIO');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, sdio_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: NodeAdd Sdio(sdio_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: NodeAdd Sdio(sdio_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  // Create the wifi composite device that will be the parent for wifi devices.
  fpbus::Node wifi_dev;
  wifi_dev.name() = "wifi";
  wifi_dev.vid() = PDEV_VID_NXP;
  wifi_dev.pid() = PDEV_PID_MARVELL_88W8987;
  wifi_dev.did() = PDEV_DID_MARVELL_WIFI;
  wifi_dev.metadata() = wifi_metadata;
  wifi_dev.boot_metadata() = wifi_boot_metadata;

  fidl_arena.Reset();
  auto fragment = platform_bus_composite::MakeFidlFragment(fidl_arena, wifi_fragments,
                                                           std::size(wifi_fragments));
  arena = fdf::Arena('WIFI');
  auto res = pbus_.buffer(arena)->AddComposite(fidl::ToWire(fidl_arena, wifi_dev), fragment,
                                               "sdio-function-1");
  if (!res.ok()) {
    zxlogf(ERROR, "Request to add WIFI composite failed: %s", res.FormatDescription().data());
    return res.status();
  }
  if (res->is_error()) {
    zxlogf(ERROR, "Failed to add WIFI composite: %s", zx_status_get_string(res->error_value()));
    return res->error_value();
  }

  return status;
}

}  // namespace board_pinecrest
