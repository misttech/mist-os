// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sdmmc/cpp/wire.h>
#include <fuchsia/hardware/sdmmc/c/banjo.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-common/aml-sdmmc.h>

#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"
#include "vim3.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> sd_mmios{
    {{
        .base = A311D_EMMC_B_BASE,
        .length = A311D_EMMC_B_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> sd_irqs{
    {{
        .irq = A311D_SD_EMMC_B_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Bti> sd_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_SD,
    }},
};

static aml_sdmmc_config_t config = {
    .min_freq = 400'000,
    .max_freq = 50'000'000,
    .version_3 = true,
    .prefs = 0,
};

const std::vector<fdf::BindRule> kGpioInitRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

const std::vector<fdf::NodeProperty> kGpioInitProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

zx_status_t Vim3::SdInit() {
  fidl::Arena<> fidl_arena;

  fit::result sdmmc_metadata =
      fidl::Persist(fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(fidl_arena)
                        .removable(true)
                        // TODO(https://fxbug.dev/42084501): Use the FIDL SDMMC protocol.
                        .use_fidl(false)
                        .Build());
  if (!sdmmc_metadata.is_ok()) {
    zxlogf(ERROR, "Failed to encode SDMMC metadata: %s",
           sdmmc_metadata.error_value().FormatDescription().c_str());
    return sdmmc_metadata.error_value().status();
  }

  const std::vector<fpbus::Metadata> sd_metadata{
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&config),
                                       reinterpret_cast<const uint8_t*>(&config) + sizeof(config)),
      }},
      {{
          .type = DEVICE_METADATA_SDMMC,
          .data = std::move(sdmmc_metadata.value()),
      }},
  };

  fpbus::Node sd_dev;
  sd_dev.name() = "aml_sd";
  sd_dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  sd_dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  sd_dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_SDMMC_B;
  sd_dev.mmio() = sd_mmios;
  sd_dev.irq() = sd_irqs;
  sd_dev.bti() = sd_btis;
  sd_dev.metadata() = sd_metadata;

  gpio_init_steps_.push_back({A311D_GPIOC(0), GpioSetAltFunction(A311D_GPIOC_0_SDCARD_D0_FN)});
  gpio_init_steps_.push_back({A311D_GPIOC(1), GpioSetAltFunction(A311D_GPIOC_1_SDCARD_D1_FN)});
  gpio_init_steps_.push_back({A311D_GPIOC(2), GpioSetAltFunction(A311D_GPIOC_2_SDCARD_D2_FN)});
  gpio_init_steps_.push_back({A311D_GPIOC(3), GpioSetAltFunction(A311D_GPIOC_3_SDCARD_D3_FN)});
  gpio_init_steps_.push_back({A311D_GPIOC(4), GpioSetAltFunction(A311D_GPIOC_4_SDCARD_CLK_FN)});
  gpio_init_steps_.push_back({A311D_GPIOC(5), GpioSetAltFunction(A311D_GPIOC_5_SDCARD_CMD_FN)});

  std::vector<fdf::ParentSpec> kSdParents = {
      fdf::ParentSpec{{kGpioInitRules, kGpioInitProperties}}};

  fdf::Arena arena('SD__');
  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, sd_dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "aml_sd", .parents = kSdParents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Sd(sd_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Sd(sd_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace vim3
