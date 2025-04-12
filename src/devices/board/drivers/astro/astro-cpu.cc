// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/google/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/clock/cpp/bind.h>
#include <bind/fuchsia/hardware/power/cpp/bind.h>
#include <bind/fuchsia/power/cpp/bind.h>
#include <soc/aml-common/aml-cpu-metadata.h>
#include <soc/aml-meson/g12a-clk.h>
#include <soc/aml-s905d2/s905d2-hw.h>

#include "astro-gpios.h"
#include "astro.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace {
namespace fpbus = fuchsia_hardware_platform_bus;

constexpr amlogic_cpu::PerfDomainId kPdArmA53 = 1;

const std::vector<fpbus::Mmio> cpu_mmios{
    {{
        // AOBUS
        .base = S905D2_AOBUS_BASE,
        .length = S905D2_AOBUS_LENGTH,
    }},
};

constexpr amlogic_cpu::operating_point_t operating_points[] = {
    {.freq_hz = 100'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 250'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 500'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 667'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'000'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'200'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'398'000'000, .volt_uv = 761'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'512'000'000, .volt_uv = 791'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'608'000'000, .volt_uv = 831'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'704'000'000, .volt_uv = 861'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'896'000'000, .volt_uv = 1'022'000, .pd_id = kPdArmA53},
};

constexpr amlogic_cpu::perf_domain_t performance_domains[] = {
    {.id = kPdArmA53, .core_count = 4, .relative_performance = 255, .name = "s905d2-arm-a53"},
};

static const std::vector<fpbus::Metadata> cpu_metadata{
    {{
        .id = std::to_string(DEVICE_METADATA_AML_OP_POINTS),
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&operating_points),
            reinterpret_cast<const uint8_t*>(&operating_points) + sizeof(operating_points)),
    }},
    {{
        .id = std::to_string(DEVICE_METADATA_AML_PERF_DOMAINS),
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&performance_domains),
            reinterpret_cast<const uint8_t*>(&performance_domains) + sizeof(performance_domains)),
    }},
};

const std::vector<fdf::BindRule> kPowerRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_power::SERVICE,
                            bind_fuchsia_hardware_power::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia_power::POWER_DOMAIN,
                            bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG),
};

const std::vector<fdf::NodeProperty> kPowerProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia_hardware_power::SERVICE,
                      bind_fuchsia_hardware_power::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_power::POWER_DOMAIN,
                      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG),
};

const std::vector<fdf::BindRule> kGpioInitRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};
const std::vector<fdf::NodeProperty> kGpioInitProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

// Contains all the clock parent nodes for the composite. Maps the clock id to the clock function.
const std::map<uint32_t, std::string> kClockFunctionMap = {
    {g12a_clk::CLK_SYS_PLL_DIV16, bind_fuchsia_clock::FUNCTION_SYS_PLL_DIV16},
    {g12a_clk::CLK_SYS_CPU_CLK_DIV16, bind_fuchsia_clock::FUNCTION_SYS_CPU_DIV16},
    {g12a_clk::CLK_SYS_CPU_CLK, bind_fuchsia_clock::FUNCTION_SYS_CPU_BIG_CLK},
};

static const fpbus::Node cpu_dev = []() {
  fpbus::Node result = {};
  result.name() = "aml-cpu";
  result.vid() = bind_fuchsia_google_platform::BIND_PLATFORM_DEV_VID_GOOGLE;
  result.pid() = bind_fuchsia_google_platform::BIND_PLATFORM_DEV_PID_ASTRO;
  result.did() = bind_fuchsia_google_platform::BIND_PLATFORM_DEV_DID_GOOGLE_AMLOGIC_CPU;
  result.metadata() = cpu_metadata;
  result.mmio() = cpu_mmios;
  return result;
}();

}  // namespace

namespace astro {

zx_status_t Astro::CpuInit() {
  gpio_init_steps_.push_back(GpioOutput(S905D2_PWM_D_PIN, false));

  // Configure the GPIO to be Output & set it to alternate
  // function 3 which puts in PWM_D mode.
  gpio_init_steps_.push_back(GpioFunction(S905D2_PWM_D_PIN, S905D2_PWM_D_FN));

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('CPU_');

  std::vector<fdf::ParentSpec> parents;
  parents.reserve(kClockFunctionMap.size() + 2);
  parents.push_back(fdf::ParentSpec{{kPowerRules, kPowerProperties}});
  parents.push_back(fdf::ParentSpec{{kGpioInitRules, kGpioInitProperties}});

  for (auto& [clock_id, function] : kClockFunctionMap) {
    auto rules = std::vector{
        fdf::MakeAcceptBindRule(bind_fuchsia_hardware_clock::SERVICE,
                                bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
        fdf::MakeAcceptBindRule(bind_fuchsia::CLOCK_ID, clock_id),
    };
    auto properties = std::vector{
        fdf::MakeProperty(bind_fuchsia_hardware_clock::SERVICE,
                          bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
        fdf::MakeProperty(bind_fuchsia_clock::FUNCTION, function),
    };
    parents.push_back(fdf::ParentSpec{{rules, properties}});
  }

  auto composite_result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, cpu_dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "aml_cpu", .parents = parents}}));

  if (!composite_result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec request failed: %s",
           composite_result.FormatDescription().data());
    return composite_result.status();
  }
  if (composite_result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec failed: %s",
           zx_status_get_string(composite_result->error_value()));
    return composite_result->error_value();
  }
  return ZX_OK;
}

}  // namespace astro
