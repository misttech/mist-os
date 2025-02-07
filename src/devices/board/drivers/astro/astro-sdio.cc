// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sdmmc/cpp/wire.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/mmio/mmio.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zircon-internal/align.h>

#include <bind/fuchsia/broadcom/platform/cpp/bind.h>
#include <bind/fuchsia/broadcom/platform/sdio/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/sdio/cpp/bind.h>
#include <bind/fuchsia/pwm/cpp/bind.h>
#include <bind/fuchsia/sdio/cpp/bind.h>
#include <fbl/algorithm.h>
#include <soc/aml-common/aml-sdmmc.h>
#include <soc/aml-s905d2/s905d2-gpio.h>
#include <soc/aml-s905d2/s905d2-hw.h>
#include <wifi/wifi-config.h>

#include "astro-gpios.h"
#include "astro.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace astro {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

constexpr uint32_t kGpioBase = fbl::round_down<uint32_t, uint32_t>(S905D2_GPIO_BASE, PAGE_SIZE);
constexpr uint32_t kGpioBaseOffset = S905D2_GPIO_BASE - kGpioBase;

}  // namespace

static const std::vector<fpbus::BootMetadata> wifi_boot_metadata{
    {{
        .zbi_type = ZBI_TYPE_DRV_MAC_ADDRESS,
        .zbi_extra = MACADDR_WIFI,
    }},
};

static const std::vector<fpbus::Mmio> sd_emmc_mmios{
    {{
        .base = S905D2_EMMC_B_SDIO_BASE,
        .length = S905D2_EMMC_B_SDIO_LENGTH,
    }},
    {{
        .base = S905D2_GPIO_BASE,
        .length = S905D2_GPIO_LENGTH,
    }},
    {{
        .base = S905D2_HIU_BASE,
        .length = S905D2_HIU_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> sd_emmc_irqs{
    {{
        .irq = S905D2_EMMC_B_SDIO_IRQ,
        .mode = fpbus::ZirconInterruptMode::kLevelHigh,
    }},
};

static const std::vector<fpbus::Bti> sd_emmc_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_SDIO,
    }},
};

static const wifi_config_t wifi_config = {
    .oob_irq_mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    .iovar_table =
        {
            {IOVAR_STR_TYPE, {"ampdu_ba_wsize"}, 32},
            {IOVAR_CMD_TYPE, {.iovar_cmd = BRCMF_C_SET_PM}, 0},
            {IOVAR_CMD_TYPE, {.iovar_cmd = BRCMF_C_SET_FAKEFRAG}, 1},
            {IOVAR_LIST_END_TYPE, {{0}}, 0},
        },
    .cc_table =
        {
            {"WW", 0},   {"AU", 922}, {"CA", 900}, {"US", 842}, {"GB", 888}, {"BE", 888},
            {"BG", 888}, {"CZ", 888}, {"DK", 888}, {"DE", 888}, {"EE", 888}, {"IE", 888},
            {"GR", 888}, {"ES", 888}, {"FR", 888}, {"HR", 888}, {"IT", 888}, {"CY", 888},
            {"LV", 888}, {"LT", 888}, {"LU", 888}, {"HU", 888}, {"MT", 888}, {"NL", 888},
            {"AT", 888}, {"PL", 888}, {"PT", 888}, {"RO", 888}, {"SI", 888}, {"SK", 888},
            {"FI", 888}, {"SE", 888}, {"EL", 888}, {"IS", 888}, {"LI", 888}, {"TR", 888},
            {"JP", 1},   {"KR", 1},   {"TW", 1},   {"NO", 1},   {"IN", 1},   {"SG", 1},
            {"MX", 1},   {"NZ", 1},   {"CH", 1},   {"", 0},
        },
};

static const std::vector<fpbus::Metadata> wifi_metadata{
    {{
        .id = std::to_string(DEVICE_METADATA_WIFI_CONFIG),
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
    fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(GPIO_SDIO_RESET)),
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

zx_status_t Astro::SdEmmcConfigurePortB() {
  size_t aligned_size = ZX_ROUNDUP((S905D2_GPIO_BASE - kGpioBase) + S905D2_GPIO_LENGTH, PAGE_SIZE);
  zx::unowned_resource resource(get_mmio_resource(parent()));
  zx::vmo vmo;
  zx_status_t status = zx::vmo::create_physical(*resource, kGpioBase, aligned_size, &vmo);
  if (status != ZX_OK) {
    zxlogf(ERROR, "failed to create VMO: %s", zx_status_get_string(status));
    return status;
  }

  zx::result<fdf::MmioBuffer> gpio_base =
      fdf::MmioBuffer::Create(0, aligned_size, std::move(vmo), ZX_CACHE_POLICY_UNCACHED_DEVICE);
  if (gpio_base.is_error()) {
    zxlogf(ERROR, "Create(gpio) error: %s", gpio_base.status_string());
  }

  // TODO(https://fxbug.dev/42155334): Figure out if we need gpio protocol ops to modify these
  // gpio registers.

  // PREG_PAD_GPIO5_O[17] is an undocumented bit that selects between (0) GPIO and (1) SDMMC port B
  // as outputs to GPIOX_4 (the SDIO clock pin). This mux is upstream of the alt function mux, so in
  // order for port B to use GPIOX, the alt function value must also be set to zero. Note that the
  // output enable signal does not seem to be muxed here, and must be set separately in order for
  // clock output to work.
  gpio_base->SetBits32(AML_SDIO_PORTB_GPIO_REG_5_VAL,
                       kGpioBaseOffset + (S905D2_PREG_PAD_GPIO5_O << 2));

  // PERIPHS_PIN_MUX_2[24] is another undocumented bit that controls the corresponding mux for the
  // rest of the SDIO pins (data and cmd). Unlike GPIOX_4, the output enable signals are also muxed,
  // so the pin directions don't need to be set manually.
  gpio_base->SetBits32(AML_SDIO_PORTB_PERIPHS_PINMUX2_VAL,
                       kGpioBaseOffset + (S905D2_PERIPHS_PIN_MUX_2 << 2));

  auto sdio_pin = [](uint32_t pin) {
    return fuchsia_hardware_pinimpl::InitStep::WithCall({{
        .pin = pin,
        .call = fuchsia_hardware_pinimpl::InitCall::WithPinConfig({{
            .function = 0,
            .drive_strength_ua = 4'000,
        }}),
    }});
  };

  // Clear GPIO_X
  gpio_init_steps_.push_back(sdio_pin(S905D2_WIFI_SDIO_D0));
  gpio_init_steps_.push_back(sdio_pin(S905D2_WIFI_SDIO_D1));
  gpio_init_steps_.push_back(sdio_pin(S905D2_WIFI_SDIO_D2));
  gpio_init_steps_.push_back(sdio_pin(S905D2_WIFI_SDIO_D3));
  gpio_init_steps_.push_back(sdio_pin(S905D2_WIFI_SDIO_CLK));
  gpio_init_steps_.push_back(sdio_pin(S905D2_WIFI_SDIO_CMD));

  // Clear GPIO_C
  gpio_init_steps_.push_back(GpioFunction(S905D2_GPIOC(0), 0));
  gpio_init_steps_.push_back(GpioFunction(S905D2_GPIOC(1), 0));
  gpio_init_steps_.push_back(GpioFunction(S905D2_GPIOC(2), 0));
  gpio_init_steps_.push_back(GpioFunction(S905D2_GPIOC(3), 0));
  gpio_init_steps_.push_back(GpioFunction(S905D2_GPIOC(4), 0));
  gpio_init_steps_.push_back(GpioFunction(S905D2_GPIOC(5), 0));

  // Enable output from SDMMC port B on GPIOX_4.
  gpio_init_steps_.push_back(GpioOutput(S905D2_WIFI_SDIO_CLK, true));

  gpio_init_steps_.push_back(GpioFunction(S905D2_WIFI_SDIO_WAKE_HOST, 0));

  // Configure clock settings
  status = zx::vmo::create_physical(*resource, S905D2_HIU_BASE, S905D2_HIU_LENGTH, &vmo);
  if (status != ZX_OK) {
    zxlogf(ERROR, "failed to create VMO: %s", zx_status_get_string(status));
    return status;
  }
  zx::result<fdf::MmioBuffer> hiu_base = fdf::MmioBuffer::Create(
      0, S905D2_HIU_LENGTH, std::move(vmo), ZX_CACHE_POLICY_UNCACHED_DEVICE);
  if (hiu_base.is_error()) {
    zxlogf(ERROR, "Create(hiu) error: %s", hiu_base.status_string());
  }

  uint32_t hhi_gclock_val =
      hiu_base->Read32(HHI_GCLK_MPEG0_OFFSET << 2) | AML_SDIO_PORTB_HHI_GCLK_MPEG0_VAL;
  hiu_base->Write32(hhi_gclock_val, HHI_GCLK_MPEG0_OFFSET << 2);

  uint32_t hh1_sd_emmc_clock_val =
      hiu_base->Read32(HHI_SD_EMMC_CLK_CNTL_OFFSET << 2) & AML_SDIO_PORTB_SDMMC_CLK_VAL;
  hiu_base->Write32(hh1_sd_emmc_clock_val, HHI_SD_EMMC_CLK_CNTL_OFFSET << 2);

  return status;
}

zx_status_t AddWifiComposite(fdf::WireSyncClient<fpbus::PlatformBus>& pbus,
                             fidl::AnyArena& fidl_arena, fdf::Arena& arena) {
  const std::vector<fdf::BindRule> kGpioWifiHostRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                              bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              static_cast<uint32_t>(S905D2_WIFI_SDIO_WAKE_HOST)),
  };

  const std::vector<fdf::NodeProperty> kGpioWifiHostProperties = std::vector{
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                        bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
  };

  fpbus::Node wifi_dev;
  wifi_dev.name() = "wifi";
  wifi_dev.vid() = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM;
  wifi_dev.pid() = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458;
  wifi_dev.did() = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_DID_WIFI;
  wifi_dev.metadata() = wifi_metadata;
  wifi_dev.metadata() = wifi_metadata;
  wifi_dev.boot_metadata() = wifi_boot_metadata;

  constexpr uint32_t kSdioFunctionCount = 2;
  std::vector<fdf::ParentSpec> wifi_parents = {
      fdf::ParentSpec{{kGpioWifiHostRules, kGpioWifiHostProperties}},
      fdf::ParentSpec{{kGpioInitRules, kGpioInitProperties}},
  };
  wifi_parents.reserve(wifi_parents.size() + kSdioFunctionCount);
  for (uint32_t i = 1; i <= kSdioFunctionCount; i++) {
    auto sdio_bind_rules = {
        fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_sdio::BIND_PROTOCOL_DEVICE),
        fdf::MakeAcceptBindRule(bind_fuchsia::SDIO_VID,
                                bind_fuchsia_broadcom_platform_sdio::BIND_SDIO_VID_BROADCOM),
        fdf::MakeAcceptBindRule(bind_fuchsia::SDIO_PID,
                                bind_fuchsia_broadcom_platform_sdio::BIND_SDIO_PID_BCM4345),
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

zx_status_t Astro::SdioInit() {
  fidl::Arena<> fidl_arena;

  fit::result sdmmc_metadata =
      fidl::Persist(fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(fidl_arena)
                        .max_frequency(208'000'000)
                        // TODO(https://fxbug.dev/42084501): Use the FIDL SDMMC protocol.
                        .use_fidl(false)
                        .Build());
  if (!sdmmc_metadata.is_ok()) {
    zxlogf(ERROR, "Failed to encode SDMMC metadata: %s",
           sdmmc_metadata.error_value().FormatDescription().c_str());
    return sdmmc_metadata.error_value().status();
  }

  const std::vector<fpbus::Metadata> sd_emmc_metadata{
      {{
          .id = fuchsia_hardware_sdmmc::wire::SdmmcMetadata::kSerializableName,
          .data = std::move(sdmmc_metadata.value()),
      }},
  };

  fpbus::Node sd_emmc_dev;
  sd_emmc_dev.name() = "aml-sdio";
  sd_emmc_dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  sd_emmc_dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  sd_emmc_dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_SDMMC_B;
  sd_emmc_dev.mmio() = sd_emmc_mmios;
  sd_emmc_dev.irq() = sd_emmc_irqs;
  sd_emmc_dev.bti() = sd_emmc_btis;
  sd_emmc_dev.metadata() = sd_emmc_metadata;

  SdEmmcConfigurePortB();

  std::vector<fdf::ParentSpec> kSdioParents = {
      fdf::ParentSpec{{kPwmRules, kPwmProperties}},
      fdf::ParentSpec{{kGpioInitRules, kGpioInitProperties}},
      fdf::ParentSpec{{kGpioResetRules, kGpioResetProperties}}};

  fdf::Arena sdio_arena('SDIO');
  auto result =
      pbus_.buffer(sdio_arena)
          ->AddCompositeNodeSpec(
              fidl::ToWire(fidl_arena, sd_emmc_dev),
              fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                           {.name = "aml_sdio", .parents = kSdioParents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Sdio(sd_emmc_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Sdio(sd_emmc_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  gpio_init_steps_.push_back(
      GpioPull(S905D2_WIFI_SDIO_WAKE_HOST, fuchsia_hardware_pin::Pull::kNone));

  fdf::Arena wifi_arena('WIFI');
  auto status = AddWifiComposite(pbus_, fidl_arena, wifi_arena);
  if (status != ZX_OK) {
    return status;
  }

  return ZX_OK;
}

}  // namespace astro
