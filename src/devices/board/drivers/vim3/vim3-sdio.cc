// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fuchsia/hardware/sdmmc/c/banjo.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-common/aml-sdmmc.h>
#include <wifi/wifi-config.h>

#include "src/devices/board/drivers/vim3/vim3-gpios.h"
#include "src/devices/board/drivers/vim3/vim3-sdio-bind.h"
#include "src/devices/board/drivers/vim3/vim3-wifi-bind.h"
#include "src/devices/board/drivers/vim3/vim3.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> sdio_mmios{
    {{
        .base = A311D_EMMC_A_BASE,
        .length = A311D_EMMC_A_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> sdio_irqs{
    {{
        .irq = A311D_SD_EMMC_A_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Bti> sdio_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_SDIO,
    }},
};

static aml_sdmmc_config_t config = {
    .supports_dma = true,
    .min_freq = 400'000,
    .max_freq = 100'000'000,
    .version_3 = true,
    .prefs = 0,
    .use_new_tuning = true,
};

constexpr wifi_config_t wifi_config = {
    .oob_irq_mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    .clm_needed = false,
    .iovar_table =
        {
            {IOVAR_CMD_TYPE, {.iovar_cmd = BRCMF_C_SET_PM}, 0},
            {IOVAR_LIST_END_TYPE, {{0}}, 0},
        },
    .cc_table =
        {
            {"WW", 1},   {"AU", 923}, {"CA", 901}, {"US", 843}, {"GB", 889}, {"BE", 889},
            {"BG", 889}, {"CZ", 889}, {"DK", 889}, {"DE", 889}, {"EE", 889}, {"IE", 889},
            {"GR", 889}, {"ES", 889}, {"FR", 889}, {"HR", 889}, {"IT", 889}, {"CY", 889},
            {"LV", 889}, {"LT", 889}, {"LU", 889}, {"HU", 889}, {"MT", 889}, {"NL", 889},
            {"AT", 889}, {"PL", 889}, {"PT", 889}, {"RO", 889}, {"SI", 889}, {"SK", 889},
            {"FI", 889}, {"SE", 889}, {"EL", 889}, {"IS", 889}, {"LI", 889}, {"TR", 889},
            {"CH", 889}, {"NO", 889}, {"JP", 2},   {"", 0},
        },
};

static const std::vector<fpbus::Metadata> sdio_metadata{
    {{
        .type = DEVICE_METADATA_PRIVATE,
        .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&config),
                                     reinterpret_cast<const uint8_t*>(&config) + sizeof(config)),
    }},
    {{
        .type = DEVICE_METADATA_WIFI_CONFIG,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&wifi_config),
            reinterpret_cast<const uint8_t*>(&wifi_config) + sizeof(wifi_config)),
    }},
};

zx_status_t Vim3::SdioInit() {
  zx_status_t status;

  fpbus::Node sdio_dev;
  sdio_dev.name() = "vim3-sdio";
  sdio_dev.vid() = PDEV_VID_AMLOGIC;
  sdio_dev.pid() = PDEV_PID_GENERIC;
  sdio_dev.did() = PDEV_DID_AMLOGIC_SDMMC_A;
  sdio_dev.mmio() = sdio_mmios;
  sdio_dev.irq() = sdio_irqs;
  sdio_dev.bti() = sdio_btis;
  sdio_dev.metadata() = sdio_metadata;

  gpio_impl_.ConfigIn(A311D_SDIO_D0, GPIO_NO_PULL);
  gpio_impl_.ConfigIn(A311D_SDIO_D1, GPIO_NO_PULL);
  gpio_impl_.ConfigIn(A311D_SDIO_D2, GPIO_NO_PULL);
  gpio_impl_.ConfigIn(A311D_SDIO_D3, GPIO_NO_PULL);
  gpio_impl_.ConfigIn(A311D_SDIO_CLK, GPIO_NO_PULL);
  gpio_impl_.ConfigIn(A311D_SDIO_CMD, GPIO_NO_PULL);

  gpio_impl_.SetAltFunction(A311D_SDIO_D0, A311D_GPIOX_0_SDIO_D0_FN);
  gpio_impl_.SetAltFunction(A311D_SDIO_D1, A311D_GPIOX_1_SDIO_D1_FN);
  gpio_impl_.SetAltFunction(A311D_SDIO_D2, A311D_GPIOX_2_SDIO_D2_FN);
  gpio_impl_.SetAltFunction(A311D_SDIO_D3, A311D_GPIOX_3_SDIO_D3_FN);
  gpio_impl_.SetAltFunction(A311D_SDIO_CLK, A311D_GPIOX_4_SDIO_CLK_FN);
  gpio_impl_.SetAltFunction(A311D_SDIO_CMD, A311D_GPIOX_5_SDIO_CMD_FN);

  gpio_impl_.SetDriveStrength(A311D_SDIO_D0, 4'000, nullptr);
  gpio_impl_.SetDriveStrength(A311D_SDIO_D1, 4'000, nullptr);
  gpio_impl_.SetDriveStrength(A311D_SDIO_D2, 4'000, nullptr);
  gpio_impl_.SetDriveStrength(A311D_SDIO_D3, 4'000, nullptr);
  gpio_impl_.SetDriveStrength(A311D_SDIO_CLK, 4'000, nullptr);
  gpio_impl_.SetDriveStrength(A311D_SDIO_CMD, 4'000, nullptr);

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('SDIO');
  auto result = pbus_.buffer(arena)->AddComposite(
      fidl::ToWire(fidl_arena, sdio_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, vim3_sdio_fragments,
                                               std::size(vim3_sdio_fragments)),
      "pdev");
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddComposite Sdio(sdio_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddComposite Sdio(sdio_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  // Add a composite device for wifi driver.
  const zx_device_prop_t props[] = {
      {BIND_PLATFORM_DEV_VID, 0, PDEV_VID_BROADCOM},
      {BIND_PLATFORM_DEV_PID, 0, PDEV_PID_BCM4359},
      {BIND_PLATFORM_DEV_DID, 0, PDEV_DID_BCM_WIFI},
  };

  const composite_device_desc_t comp_desc = {
      .props = props,
      .props_count = std::size(props),
      .fragments = wifi_fragments,
      .fragments_count = std::size(wifi_fragments),
      .primary_fragment = "sdio-function-1",
      .spawn_colocated = true,
      .metadata_list = nullptr,
      .metadata_count = 0,
  };

  status = DdkAddComposite("wifi", &comp_desc);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: wifi DdkAddComposite failed: %d", __func__, status);
    return status;
  }

  return ZX_OK;
}

}  // namespace vim3
