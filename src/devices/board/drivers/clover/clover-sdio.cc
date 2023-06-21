// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <zircon/syscalls/smc.h>

#include <soc/aml-a1/a1-gpio.h>
#include <soc/aml-a1/a1-hw.h>
#include <soc/aml-common/aml-sdmmc.h>

#include "src/devices/board/drivers/clover/clover-sdio-bind.h"
#include "src/devices/board/drivers/clover/clover.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace clover {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> sdio_mmios{
    {{
        .base = A1_EMMC_A_BASE,
        .length = A1_EMMC_A_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> sdio_irqs{
    {{
        .irq = A1_SD_EMMC_A_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Bti> sdio_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_SDIO,
    }},
};

static const std::vector<fpbus::Smc> usb_smcs{
    {{
        .service_call_num_base = ARM_SMC_SERVICE_CALL_NUM_SIP_SERVICE_BASE,
        .count = ARM_SMC_SERVICE_CALL_NUM_SIP_SERVICE_LENGTH,
        .exclusive = false,
    }},
};

static aml_sdmmc_config_t config = {
    .min_freq = 400'000,
    .max_freq = 200'000'000,
    .version_3 = true,
    .prefs = 0,
};

static const std::vector<fpbus::Metadata> sdio_metadata{
    {{
        .type = DEVICE_METADATA_PRIVATE,
        .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&config),
                                     reinterpret_cast<const uint8_t*>(&config) + sizeof(config)),
    }},
};

zx_status_t Clover::SdioInit() {
  fpbus::Node sdio_dev;
  sdio_dev.name() = "aml_sdio";
  sdio_dev.vid() = PDEV_VID_AMLOGIC;
  sdio_dev.pid() = PDEV_PID_AMLOGIC_A1;
  sdio_dev.did() = PDEV_DID_AMLOGIC_SDMMC_A;
  sdio_dev.mmio() = sdio_mmios;
  sdio_dev.irq() = sdio_irqs;
  sdio_dev.bti() = sdio_btis;
  sdio_dev.smc() = usb_smcs;
  sdio_dev.metadata() = sdio_metadata;

  auto sdio_gpio = [&arena = gpio_init_arena_](
                       uint64_t alt_function) -> fuchsia_hardware_gpio_init::wire::GpioInitOptions {
    return fuchsia_hardware_gpio_init::wire::GpioInitOptions::Builder(arena)
        .alt_function(alt_function)
        .Build();
  };

  gpio_init_steps_.push_back({A1_SDIO_D0, sdio_gpio(A1_SDIO_D0_FN)});
  gpio_init_steps_.push_back({A1_SDIO_D1, sdio_gpio(A1_SDIO_D1_FN)});
  gpio_init_steps_.push_back({A1_SDIO_D2, sdio_gpio(A1_SDIO_D2_FN)});
  gpio_init_steps_.push_back({A1_SDIO_D3, sdio_gpio(A1_SDIO_D3_FN)});
  gpio_init_steps_.push_back({A1_SDIO_CLK, sdio_gpio(A1_SDIO_CLK_FN)});
  gpio_init_steps_.push_back({A1_SDIO_CMD, sdio_gpio(A1_SDIO_CMD_FN)});

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('SDIO');
  auto result = pbus_.buffer(arena)->AddComposite(
      fidl::ToWire(fidl_arena, sdio_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, clover_sdio_fragments,
                                               std::size(clover_sdio_fragments)),
      "pdev");
  if (!result.ok()) {
    zxlogf(ERROR, "AddComposite Sdio(sdio_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddComposite Sdio(sdio_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace clover
