// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <cstdint>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/clock/cpp/bind.h>
#include <bind/fuchsia/hardware/power/cpp/bind.h>
#include <bind/fuchsia/power/cpp/bind.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-a311d/a311d-power.h>
#include <soc/aml-common/aml-cpu-metadata.h>
#include <soc/aml-meson/g12b-clk.h>

#include "src/devices/board/drivers/vim3/vim3.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace {
namespace fpbus = fuchsia_hardware_platform_bus;

constexpr amlogic_cpu::PerfDomainId kPdArmA73 = 1;
constexpr amlogic_cpu::PerfDomainId kPdArmA53 = 2;

const std::vector<fpbus::Mmio> cpu_mmios{
    {{
        // AOBUS
        .base = A311D_AOBUS_BASE,
        .length = A311D_AOBUS_LENGTH,
    }},
};

// clang-format off
constexpr amlogic_cpu::operating_point_t operating_points[] = {
    // Little Cluster DVFS Table
    {.freq_hz = 1'000'000'000, .volt_uv =   761'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'200'000'000, .volt_uv =   781'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'398'000'000, .volt_uv =   811'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'512'000'000, .volt_uv =   861'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'608'000'000, .volt_uv =   901'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'704'000'000, .volt_uv =   951'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'800'000'000, .volt_uv = 1'001'000, .pd_id = kPdArmA53},

    // Big Cluster DVFS Table
    {.freq_hz = 1'000'000'000, .volt_uv =   731'000, .pd_id = kPdArmA73},
    {.freq_hz = 1'200'000'000, .volt_uv =   751'000, .pd_id = kPdArmA73},
    {.freq_hz = 1'398'000'000, .volt_uv =   771'000, .pd_id = kPdArmA73},
    {.freq_hz = 1'512'000'000, .volt_uv =   771'000, .pd_id = kPdArmA73},
    {.freq_hz = 1'608'000'000, .volt_uv =   781'000, .pd_id = kPdArmA73},
    {.freq_hz = 1'704'000'000, .volt_uv =   791'000, .pd_id = kPdArmA73},
    {.freq_hz = 1'800'000'000, .volt_uv =   831'000, .pd_id = kPdArmA73},
    {.freq_hz = 1'908'000'000, .volt_uv =   861'000, .pd_id = kPdArmA73},
    {.freq_hz = 2'016'000'000, .volt_uv =   911'000, .pd_id = kPdArmA73},
    {.freq_hz = 2'108'000'000, .volt_uv =   951'000, .pd_id = kPdArmA73},
    {.freq_hz = 2'208'000'000, .volt_uv = 1'011'000, .pd_id = kPdArmA73},
};
// clang-format on

// Note: relative_performance are placeholder values to indicate that the a53 is lower
//       performance than the a73. They currently do not represent the true relative performace
//       of the two cores.
constexpr amlogic_cpu::perf_domain_t performance_domains[] = {
    {.id = kPdArmA53, .core_count = 2, .relative_performance = 127, .name = "a311d-arm-a53"},
    {.id = kPdArmA73, .core_count = 4, .relative_performance = 255, .name = "a311d-arm-a73"},
};

const std::vector<fpbus::Metadata> cpu_metadata{
    {{
        .type = DEVICE_METADATA_AML_OP_POINTS,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(operating_points),
            reinterpret_cast<const uint8_t*>(operating_points) + sizeof(operating_points)),
    }},
    {{
        .type = DEVICE_METADATA_AML_PERF_DOMAINS,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(performance_domains),
            reinterpret_cast<const uint8_t*>(performance_domains) + sizeof(performance_domains)),
    }},
};

const fpbus::Node cpu_dev = []() {
  fpbus::Node result = {};
  result.name() = "aml-cpu";
  result.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  result.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_A311D;
  result.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_CPU;
  result.metadata() = cpu_metadata;
  result.mmio() = cpu_mmios;
  return result;
}();

const std::vector<uint32_t> kCpuPowerDomains = {
    bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE,
    bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG,
};

const std::map<uint32_t, std::string> kClockFunctionMap = {
    {g12b_clk::G12B_CLK_SYS_PLL_DIV16, bind_fuchsia_clock::FUNCTION_SYS_PLL_DIV16},
    {g12b_clk::G12B_CLK_SYS_CPU_CLK_DIV16, bind_fuchsia_clock::FUNCTION_SYS_CPU_DIV16},
    {g12b_clk::CLK_SYS_CPU_BIG_CLK, bind_fuchsia_clock::FUNCTION_SYS_CPU_BIG_CLK},
    {g12b_clk::G12B_CLK_SYS_PLLB_DIV16, bind_fuchsia_clock::FUNCTION_SYS_PLLB_DIV16},
    {g12b_clk::G12B_CLK_SYS_CPUB_CLK_DIV16, bind_fuchsia_clock::FUNCTION_SYS_CPUB_DIV16},
    {g12b_clk::CLK_SYS_CPU_LITTLE_CLK, bind_fuchsia_clock::FUNCTION_SYS_CPU_LITTLE_CLK},
};

}  // namespace

namespace vim3 {

zx_status_t Vim3::CpuInit() {
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('CPU_');

  std::vector<fdf::ParentSpec> parents;
  parents.reserve(kClockFunctionMap.size() + kCpuPowerDomains.size());

  for (auto& domain : kCpuPowerDomains) {
    auto power_rules = std::vector{
        fdf::MakeAcceptBindRule(bind_fuchsia_hardware_power::SERVICE,
                                bind_fuchsia_hardware_power::SERVICE_ZIRCONTRANSPORT),
        fdf::MakeAcceptBindRule(bind_fuchsia_power::POWER_DOMAIN, domain),
    };
    auto power_properties = std::vector{
        fdf::MakeProperty(bind_fuchsia_hardware_power::SERVICE,
                          bind_fuchsia_hardware_power::SERVICE_ZIRCONTRANSPORT),
        fdf::MakeProperty(bind_fuchsia_power::POWER_DOMAIN, domain),
    };
    parents.push_back(fdf::ParentSpec{{.bind_rules = power_rules, .properties = power_properties}});
  }

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

  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, cpu_dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "aml_cpu", .parents = parents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "Cpu(cpu_dev)Init: AddCompositeNodeSpec Cpu(cpu_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Cpu(cpu_dev)Init: AddCompositeNodeSpec Cpu(cpu_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace vim3
