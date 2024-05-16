// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/a311d/cpp/bind.h>
#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/amlogic/platform/meson/cpp/bind.h>
#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/clock/cpp/bind.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-common/aml-audio.h>
#include <soc/aml-meson/g12b-clk.h>

#include "vim3-gpios.h"
#include "vim3.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

zx_status_t Vim3::AudioInit() {
  using fuchsia_hardware_clockimpl::wire::InitCall;

  fidl::Arena<> fidl_arena;
  fdf::Arena fdf_arena('AUDI');
  static const std::vector<fpbus::Mmio> audio_mmios{
      {{
          .base = A311D_EE_AUDIO_BASE,
          .length = A311D_EE_AUDIO_LENGTH,
      }},
  };

  clock_init_steps_.push_back({g12b_clk::CLK_HIFI_PLL, InitCall::WithDisable({})});
  clock_init_steps_.push_back(
      {g12b_clk::CLK_HIFI_PLL, InitCall::WithRateHz(init_arena_, 768'000'000)});
  clock_init_steps_.push_back({g12b_clk::CLK_HIFI_PLL, InitCall::WithEnable({})});

  // PCM pin assignments.
  constexpr uint64_t kStrengthUa = 3000;
  // TDM bus A connected to BTPCM.
  gpio_init_steps_.push_back({A311D_GPIOX(11), GpioSetAltFunction(A311D_GPIOX_11_TDMA_SCLK_FN)});
  gpio_init_steps_.push_back({A311D_GPIOX(10), GpioSetAltFunction(A311D_GPIOX_10_TDMA_FS_FN)});
  gpio_init_steps_.push_back({A311D_GPIOX(9), GpioSetAltFunction(A311D_GPIOX_9_TDMA_D0_FN)});
  gpio_init_steps_.push_back({A311D_GPIOX(8), GpioSetAltFunction(A311D_GPIOX_8_TDMA_DIN1_FN)});
  gpio_init_steps_.push_back({A311D_GPIOX(11), GpioSetDriveStrength(kStrengthUa)});
  gpio_init_steps_.push_back({A311D_GPIOX(10), GpioSetDriveStrength(kStrengthUa)});
  gpio_init_steps_.push_back({A311D_GPIOX(9), GpioSetDriveStrength(kStrengthUa)});
  // GPIOX(8) is set as input, so no driver strength is set.

  // TDM bus B connected to I2SB.
  gpio_init_steps_.push_back({A311D_GPIOA(1), GpioSetAltFunction(A311D_GPIOA_1_TDMB_SCLK_FN)});
  gpio_init_steps_.push_back({A311D_GPIOA(2), GpioSetAltFunction(A311D_GPIOA_2_TDMB_FS_FN)});
  gpio_init_steps_.push_back({A311D_GPIOA(3), GpioSetAltFunction(A311D_GPIOA_3_TDMB_D0_FN)});
  gpio_init_steps_.push_back({A311D_GPIOA(4), GpioSetAltFunction(A311D_GPIOA_4_TDMB_DIN1_FN)});
  gpio_init_steps_.push_back({A311D_GPIOA(1), GpioSetDriveStrength(kStrengthUa)});
  gpio_init_steps_.push_back({A311D_GPIOA(2), GpioSetDriveStrength(kStrengthUa)});
  gpio_init_steps_.push_back({A311D_GPIOA(3), GpioSetDriveStrength(kStrengthUa)});
  // GPIOA(4) is set as input, so no driver strength is set.

  // Bind properties common across all devices.
  const std::vector<fdf::BindRule> kGpioInitRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };
  const std::vector<fdf::NodeProperty> kGpioInitProps = std::vector{
      fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };

  const std::vector<fdf::BindRule> kClockInitRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_clock::BIND_INIT_STEP_CLOCK),
  };
  const std::vector<fdf::NodeProperty> kClockInitProps = std::vector{
      fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_clock::BIND_INIT_STEP_CLOCK),
  };

  const std::vector<fdf::BindRule> kClkBindRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_clock::SERVICE,
                              bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::CLOCK_ID,
                              bind_fuchsia_amlogic_platform_meson::G12B_CLK_ID_CLK_AUDIO),
  };
  const std::vector<fdf::NodeProperty> kClkProperties = std::vector{
      fdf::MakeProperty(bind_fuchsia_hardware_clock::SERVICE,
                        bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia_clock::FUNCTION, bind_fuchsia_clock::FUNCTION_AUDIO_GATE),
  };
  const std::vector<fdf::BindRule> kHifiPllBindRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_clock::SERVICE,
                              bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::CLOCK_ID,
                              bind_fuchsia_amlogic_platform_meson::G12B_CLK_ID_CLK_HIFI_PLL),
  };
  const std::vector<fdf::NodeProperty> kHifiPllProperties = std::vector{
      fdf::MakeProperty(bind_fuchsia_hardware_clock::SERVICE,
                        bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia_clock::FUNCTION, bind_fuchsia_clock::FUNCTION_AUDIO_PLL),
  };

  const std::vector<fdf::BindRule> kTdmASclkRules{
      fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, VIM3_GPIO_ID),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(VIM3_BTPCM_CLK)),
  };
  const std::vector<fdf::NodeProperty> kTdmASclkProperties{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TDM_A_SCLK),
  };

  const std::vector<fdf::BindRule> kTdmBSclkRules{
      fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, VIM3_GPIO_ID),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(VIM3_I2SB_SCLK)),
  };
  const std::vector<fdf::NodeProperty> kTdmBSclkProperties{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TDM_B_SCLK),
  };

  std::vector<fdf::ParentSpec> kControllerParents = std::vector{
      fdf::ParentSpec{{kGpioInitRules, kGpioInitProps}},
      fdf::ParentSpec{{kClockInitRules, kClockInitProps}},
      fdf::ParentSpec{{kClkBindRules, kClkProperties}},
      fdf::ParentSpec{{kHifiPllBindRules, kHifiPllProperties}},
      fdf::ParentSpec{{kTdmASclkRules, kTdmASclkProperties}},
      fdf::ParentSpec{{kTdmBSclkRules, kTdmBSclkProperties}},
  };

  // Audio composite device setup.
  {
    static const std::vector<fpbus::Bti> composite_btis{
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_COMPOSITE,
        }},
    };

    const auto composite_spec = fdf::CompositeNodeSpec{{
        "audio-composite-composite-spec",
        kControllerParents,
    }};

    fpbus::Node tdm_dev;
    tdm_dev.name() = "audio-composite";
    tdm_dev.vid() = PDEV_VID_AMLOGIC;
    tdm_dev.pid() = PDEV_PID_AMLOGIC_A311D;
    tdm_dev.did() = PDEV_DID_AMLOGIC_AUDIO_COMPOSITE;
    tdm_dev.mmio() = audio_mmios;
    tdm_dev.bti() = composite_btis;

    // Power configuration.
    // fuchsia_hardware_power uses FIDL uint8 for power levels matching fuchsia_power_broker's.
    constexpr uint8_t kPowerLevelOff =
        static_cast<uint8_t>(fuchsia_power_broker::BinaryPowerLevel::kOff);
    constexpr uint8_t kPowerLevelOn =
        static_cast<uint8_t>(fuchsia_power_broker::BinaryPowerLevel::kOn);
    constexpr char kPowerElementName[] = "audio-hw";
    fuchsia_hardware_power::LevelTuple wake_handling_on = {{
        .child_level = kPowerLevelOn,
        .parent_level = static_cast<uint8_t>(fuchsia_power_system::ExecutionStateLevel::kActive),
    }};
    fuchsia_hardware_power::PowerDependency passive_on_execution_state = {{
        .child = kPowerElementName,
        .parent = fuchsia_hardware_power::ParentElement::WithSag(
            fuchsia_hardware_power::SagElement::kExecutionState),
        .level_deps = {{std::move(wake_handling_on)}},
        .strength = fuchsia_hardware_power::RequirementType::kPassive,
    }};
    fuchsia_hardware_power::PowerLevel off = {{.level = kPowerLevelOff, .name = "off"}};
    fuchsia_hardware_power::PowerLevel on = {{.level = kPowerLevelOn, .name = "on"}};
    fuchsia_hardware_power::PowerElement element = {
        {.name = kPowerElementName, .levels = {{std::move(off), std::move(on)}}}};
    fuchsia_hardware_power::PowerElementConfiguration config = {
        {.element = std::move(element), .dependencies = {{std::move(passive_on_execution_state)}}}};
    tdm_dev.power_config() = {std::move(config)};

    auto result = pbus_.buffer(fdf_arena)->AddCompositeNodeSpec(
        fidl::ToWire(fidl_arena, tdm_dev), fidl::ToWire(fidl_arena, composite_spec));
    if (!result.ok()) {
      zxlogf(ERROR, "AddCompositeNodeSpec failed: %s", result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddCompositeNodeSpec failed: %s", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  return ZX_OK;
}

}  // namespace vim3
