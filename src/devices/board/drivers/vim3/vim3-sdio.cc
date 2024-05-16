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
#include <bind/fuchsia/broadcom/platform/cpp/bind.h>
#include <bind/fuchsia/broadcom/platform/sdio/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/sdio/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <bind/fuchsia/pwm/cpp/bind.h>
#include <bind/fuchsia/sdio/cpp/bind.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-common/aml-sdmmc.h>
#include <wifi/wifi-config.h>

#include "src/devices/board/drivers/vim3/vim3-gpios.h"
#include "src/devices/board/drivers/vim3/vim3.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

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
            {"WW", 0}, {"AU", 0}, {"CA", 0}, {"US", 0}, {"GB", 0}, {"BE", 0}, {"BG", 0}, {"CZ", 0},
            {"DK", 0}, {"DE", 0}, {"EE", 0}, {"IE", 0}, {"GR", 0}, {"ES", 0}, {"FR", 0}, {"HR", 0},
            {"IT", 0}, {"CY", 0}, {"LV", 0}, {"LT", 0}, {"LU", 0}, {"HU", 0}, {"MT", 0}, {"NL", 0},
            {"AT", 0}, {"PL", 0}, {"PT", 0}, {"RO", 0}, {"SI", 0}, {"SK", 0}, {"FI", 0}, {"SE", 0},
            {"EL", 0}, {"IS", 0}, {"LI", 0}, {"TR", 0}, {"CH", 0}, {"NO", 0}, {"JP", 0}, {"", 0},
        },
};

static const std::vector<fpbus::Metadata> wifi_metadata{
    {{
        .type = DEVICE_METADATA_WIFI_CONFIG,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&wifi_config),
            reinterpret_cast<const uint8_t*>(&wifi_config) + sizeof(wifi_config)),
    }},
};

const std::vector<fdf::BindRule> kPwmRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_pwm::BIND_INIT_STEP_PWM),
};

const std::vector<fdf::NodeProperty> kPwmProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_pwm::BIND_INIT_STEP_PWM),
};

const std::vector<fdf::BindRule> kGpioResetRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                            bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, VIM3_GPIO_ID),
    fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(A311D_GPIOX(6))),
};

const std::vector<fdf::NodeProperty> kGpioResetProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                      bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_SDMMC_RESET),
};

const std::vector<fdf::BindRule> kGpioInitRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

const std::vector<fdf::NodeProperty> kGpioInitProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

zx_status_t AddWifiComposite(fdf::WireSyncClient<fpbus::PlatformBus>& pbus,
                             fidl::AnyArena& fidl_arena, fdf::Arena& arena) {
  const std::vector<fdf::BindRule> kGpioWifiHostRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                              bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, VIM3_GPIO_ID),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(VIM3_WIFI_WAKE_HOST)),
  };

  const std::vector<fdf::NodeProperty> kGpioWifiHostProperties = std::vector{
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                        bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
  };

  fpbus::Node wifi_dev;
  wifi_dev.name() = "wifi";
  wifi_dev.vid() = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM;
  wifi_dev.pid() = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM4359;
  wifi_dev.did() = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_DID_WIFI;
  wifi_dev.metadata() = wifi_metadata;

  constexpr uint32_t kSdioFunctionCount = 2;
  std::vector<fdf::ParentSpec> wifi_parents = {
      fdf::ParentSpec{{kGpioWifiHostRules, kGpioWifiHostProperties}}};
  wifi_parents.reserve(wifi_parents.size() + kSdioFunctionCount);
  for (uint32_t i = 1; i <= kSdioFunctionCount; i++) {
    auto sdio_bind_rules = {
        fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_sdio::BIND_PROTOCOL_DEVICE),
        fdf::MakeAcceptBindRule(bind_fuchsia::SDIO_VID,
                                bind_fuchsia_broadcom_platform_sdio::BIND_SDIO_VID_BROADCOM),
        fdf::MakeAcceptBindRule(bind_fuchsia::SDIO_PID,
                                bind_fuchsia_broadcom_platform_sdio::BIND_SDIO_PID_BCM4359),
        fdf::MakeAcceptBindRule(bind_fuchsia::SDIO_FUNCTION, i),
    };

    auto sdio_properties = {
        fdf::MakeProperty(bind_fuchsia_hardware_sdio::SERVICE,
                          bind_fuchsia_hardware_sdio::SERVICE_ZIRCONTRANSPORT),
        fdf::MakeProperty(bind_fuchsia::SDIO_FUNCTION, i),
    };

    wifi_parents.push_back(fdf::ParentSpec{
        {sdio_bind_rules, sdio_properties},
    });
  }

  fdf::WireUnownedResult result = pbus.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, wifi_dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "wifi", .parents = wifi_parents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddCompositeNodeSpec request to platform bus: %s",
           result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add wifi composite to platform device: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

zx_status_t Vim3::SdioInit() {
  fidl::Arena<> fidl_arena;

  fit::result sdmmc_metadata =
      fidl::Persist(fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(fidl_arena)
                        .max_frequency(100'000'000)
                        // TODO(https://fxbug.dev/42084501): Use the FIDL SDMMC protocol.
                        .use_fidl(false)
                        .Build());
  if (!sdmmc_metadata.is_ok()) {
    zxlogf(ERROR, "Failed to encode SDMMC metadata: %s",
           sdmmc_metadata.error_value().FormatDescription().c_str());
    return sdmmc_metadata.error_value().status();
  }

  const std::vector<fpbus::Metadata> sdio_metadata{
      {{
          .type = DEVICE_METADATA_SDMMC,
          .data = std::move(sdmmc_metadata.value()),
      }},
  };

  fpbus::Node sdio_dev;
  sdio_dev.name() = "vim3-sdio";
  sdio_dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  sdio_dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  sdio_dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_SDMMC_A;
  sdio_dev.mmio() = sdio_mmios;
  sdio_dev.irq() = sdio_irqs;
  sdio_dev.bti() = sdio_btis;
  sdio_dev.metadata() = sdio_metadata;

  using fuchsia_hardware_gpio::GpioFlags;

  gpio_init_steps_.push_back({A311D_SDIO_D0, GpioConfigIn(GpioFlags::kNoPull)});
  gpio_init_steps_.push_back({A311D_SDIO_D1, GpioConfigIn(GpioFlags::kNoPull)});
  gpio_init_steps_.push_back({A311D_SDIO_D2, GpioConfigIn(GpioFlags::kNoPull)});
  gpio_init_steps_.push_back({A311D_SDIO_D3, GpioConfigIn(GpioFlags::kNoPull)});
  gpio_init_steps_.push_back({A311D_SDIO_CLK, GpioConfigIn(GpioFlags::kNoPull)});
  gpio_init_steps_.push_back({A311D_SDIO_CMD, GpioConfigIn(GpioFlags::kNoPull)});

  gpio_init_steps_.push_back({A311D_SDIO_D0, GpioSetAltFunction(A311D_GPIOX_0_SDIO_D0_FN)});
  gpio_init_steps_.push_back({A311D_SDIO_D1, GpioSetAltFunction(A311D_GPIOX_1_SDIO_D1_FN)});
  gpio_init_steps_.push_back({A311D_SDIO_D2, GpioSetAltFunction(A311D_GPIOX_2_SDIO_D2_FN)});
  gpio_init_steps_.push_back({A311D_SDIO_D3, GpioSetAltFunction(A311D_GPIOX_3_SDIO_D3_FN)});
  gpio_init_steps_.push_back({A311D_SDIO_CLK, GpioSetAltFunction(A311D_GPIOX_4_SDIO_CLK_FN)});
  gpio_init_steps_.push_back({A311D_SDIO_CMD, GpioSetAltFunction(A311D_GPIOX_5_SDIO_CMD_FN)});

  gpio_init_steps_.push_back({A311D_SDIO_D0, GpioSetDriveStrength(4'000)});
  gpio_init_steps_.push_back({A311D_SDIO_D1, GpioSetDriveStrength(4'000)});
  gpio_init_steps_.push_back({A311D_SDIO_D2, GpioSetDriveStrength(4'000)});
  gpio_init_steps_.push_back({A311D_SDIO_D3, GpioSetDriveStrength(4'000)});
  gpio_init_steps_.push_back({A311D_SDIO_CLK, GpioSetDriveStrength(4'000)});
  gpio_init_steps_.push_back({A311D_SDIO_CMD, GpioSetDriveStrength(4'000)});

  std::vector<fdf::ParentSpec> kSdioParents = {
      fdf::ParentSpec{{kPwmRules, kPwmProperties}},
      fdf::ParentSpec{{kGpioInitRules, kGpioInitProperties}},
      fdf::ParentSpec{{kGpioResetRules, kGpioResetProperties}}};

  fdf::Arena sdio_arena('SDIO');
  auto result =
      pbus_.buffer(sdio_arena)
          ->AddCompositeNodeSpec(
              fidl::ToWire(fidl_arena, sdio_dev),
              fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                           {.name = "vim3_sdio", .parents = kSdioParents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Sdio(sdio_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Sdio(sdio_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  // Add a composite device for wifi driver.
  fdf::Arena wifi_arena('WIFI');
  auto status = AddWifiComposite(pbus_, fidl_arena, wifi_arena);
  if (status != ZX_OK) {
    return status;
  }

  return ZX_OK;
}

}  // namespace vim3
