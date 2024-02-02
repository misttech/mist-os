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
#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-common/aml-sdmmc.h>
#include <soc/aml-meson/g12b-clk.h>

#include "src/devices/board/drivers/vim3/vim3.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;
#define BIT_MASK(start, count) (((1 << (count)) - 1) << (start))
#define SET_BITS(dest, start, count, value) \
  ((dest & ~BIT_MASK(start, count)) | (((value) << (start)) & BIT_MASK(start, count)))

static const std::vector<fpbus::Mmio> emmc_mmios{
    {{
        .base = A311D_EMMC_C_BASE,
        .length = A311D_EMMC_C_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> emmc_irqs{
    {{
        .irq = A311D_SD_EMMC_C_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Bti> emmc_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_EMMC,
    }},
};

static aml_sdmmc_config_t config = {
    .min_freq = 400000,
    .max_freq = 120000000,
    .version_3 = true,
    .prefs = SDMMC_HOST_PREFS_DISABLE_HS400,
};

static const std::vector<fpbus::BootMetadata> emmc_boot_metadata{
    {{
        .zbi_type = DEVICE_METADATA_PARTITION_MAP,
        .zbi_extra = 0,
    }},
};

const std::vector<fdf::BindRule> kClockGateRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_clock::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeAcceptBindRule(bind_fuchsia::CLOCK_ID, g12b_clk::G12B_CLK_EMMC_C),
};

const std::vector<fdf::NodeProperty> kClockGateProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_clock::BIND_FIDL_PROTOCOL_SERVICE),
};

const std::vector<fdf::BindRule> kGpioResetRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(A311D_GPIOBOOT(12))),
};

const std::vector<fdf::NodeProperty> kGpioResetProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_SDMMC_RESET),
};

const std::vector<fdf::BindRule> kGpioInitRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

const std::vector<fdf::NodeProperty> kGpioInitProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

zx_status_t Vim3::EmmcInit() {
  fidl::Arena<> fidl_arena;

  const std::vector<fpbus::Metadata> emmc_metadata{
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&config),
                                       reinterpret_cast<const uint8_t*>(&config) + sizeof(config)),
      }},
  };

  fpbus::Node emmc_dev;
  emmc_dev.name() = "aml_emmc";
  emmc_dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  emmc_dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  emmc_dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_SDMMC_C;
  emmc_dev.mmio() = emmc_mmios;
  emmc_dev.irq() = emmc_irqs;
  emmc_dev.bti() = emmc_btis;
  emmc_dev.metadata() = emmc_metadata;
  emmc_dev.boot_metadata() = emmc_boot_metadata;

  // set alternate functions to enable EMMC
  gpio_init_steps_.push_back({A311D_GPIOBOOT(0), GpioSetAltFunction(A311D_GPIOBOOT_0_EMMC_D0_FN)});
  gpio_init_steps_.push_back({A311D_GPIOBOOT(1), GpioSetAltFunction(A311D_GPIOBOOT_1_EMMC_D1_FN)});
  gpio_init_steps_.push_back({A311D_GPIOBOOT(2), GpioSetAltFunction(A311D_GPIOBOOT_2_EMMC_D2_FN)});
  gpio_init_steps_.push_back({A311D_GPIOBOOT(3), GpioSetAltFunction(A311D_GPIOBOOT_3_EMMC_D3_FN)});
  gpio_init_steps_.push_back({A311D_GPIOBOOT(4), GpioSetAltFunction(A311D_GPIOBOOT_4_EMMC_D4_FN)});
  gpio_init_steps_.push_back({A311D_GPIOBOOT(5), GpioSetAltFunction(A311D_GPIOBOOT_5_EMMC_D5_FN)});
  gpio_init_steps_.push_back({A311D_GPIOBOOT(6), GpioSetAltFunction(A311D_GPIOBOOT_6_EMMC_D6_FN)});
  gpio_init_steps_.push_back({A311D_GPIOBOOT(7), GpioSetAltFunction(A311D_GPIOBOOT_7_EMMC_D7_FN)});
  gpio_init_steps_.push_back({A311D_GPIOBOOT(8), GpioSetAltFunction(A311D_GPIOBOOT_8_EMMC_CLK_FN)});
  gpio_init_steps_.push_back(
      {A311D_GPIOBOOT(10), GpioSetAltFunction(A311D_GPIOBOOT_10_EMMC_CMD_FN)});
  // gpio_init_steps_.push_back({A311D_GPIOBOOT(12), GpioSetAltFunction(1)});
  gpio_init_steps_.push_back(
      {A311D_GPIOBOOT(13), GpioSetAltFunction(A311D_GPIOBOOT_13_EMMC_DS_FN)});

  gpio_init_steps_.push_back({A311D_GPIOBOOT(14), GpioConfigOut(1)});

  std::vector<fdf::ParentSpec> kEmmcParents = {
      fdf::ParentSpec{{kClockGateRules, kClockGateProperties}},
      fdf::ParentSpec{{kGpioInitRules, kGpioInitProperties}},
      fdf::ParentSpec{{kGpioResetRules, kGpioResetProperties}}};

  fdf::Arena arena('EMMC');
  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, emmc_dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "aml_emmc", .parents = kEmmcParents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Emmc(emmc_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Emmc(emmc_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}
}  // namespace vim3
