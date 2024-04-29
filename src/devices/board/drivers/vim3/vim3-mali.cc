// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/arm/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/gpu/mali/cpp/bind.h>
#include <bind/fuchsia/hardware/registers/cpp/bind.h>
#include <bind/fuchsia/register/cpp/bind.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-common/aml-registers.h>

#include "src/devices/board/drivers/vim3/vim3.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;
static const std::vector<fpbus::Mmio> aml_gpu_mmios{
    {{
        .base = A311D_MALI_BASE,
        .length = A311D_MALI_LENGTH,
    }},
    {{
        .base = A311D_HIU_BASE,
        .length = A311D_HIU_LENGTH,
    }},
};

static const std::vector<fpbus::Mmio> mali_mmios{
    {{
        .base = A311D_MALI_BASE,
        .length = A311D_MALI_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> mali_irqs{
    {{
        .irq = A311D_MALI_IRQ_PP,
        .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    }},
    {{
        .irq = A311D_MALI_IRQ_GPMMU,
        .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    }},
    {{
        .irq = A311D_MALI_IRQ_GP,
        .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    }},
};

static const std::vector<fpbus::Bti> mali_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_MALI,
    }},
};

namespace {
constexpr fuchsia_power_broker::PowerLevel kPowerLevelOff = 0;
constexpr fuchsia_power_broker::PowerLevel kPowerLevelOn = 1;

// This power element represents the GPU hardware. Its passive dependency on SAG's (Execution State,
// wake handling) allows for orderly power down of the hardware before the CPU suspends scheduling.
fuchsia_hardware_power::PowerElementConfiguration hardware_power_config() {
  constexpr char kPowerElementName[] = "mali-gpu-hardware";

  auto transitions_from_off =
      std::vector<fuchsia_hardware_power::Transition>{fuchsia_hardware_power::Transition{{
          .target_level = kPowerLevelOn,
          .latency_us = 500,
      }}};
  auto transitions_from_on =
      std::vector<fuchsia_hardware_power::Transition>{fuchsia_hardware_power::Transition{{
          .target_level = kPowerLevelOff,
          .latency_us = 2000,
      }}};
  fuchsia_hardware_power::PowerLevel off = {
      {.level = kPowerLevelOff, .name = "off", .transitions = transitions_from_off}};
  fuchsia_hardware_power::PowerLevel on = {
      {.level = kPowerLevelOn, .name = "on", .transitions = transitions_from_on}};
  fuchsia_hardware_power::PowerElement hardware_power = {{
      .name = kPowerElementName,
      .levels = {{off, on}},
  }};

  fuchsia_hardware_power::LevelTuple on_to_wake_handling = {{
      .child_level = kPowerLevelOn,
      .parent_level =
          static_cast<uint8_t>(fuchsia_power_system::ExecutionStateLevel::kWakeHandling),
  }};
  fuchsia_hardware_power::PowerDependency passive_on_exec_state_wake_handling = {{
      .child = kPowerElementName,
      .parent = fuchsia_hardware_power::ParentElement::WithSag(
          fuchsia_hardware_power::SagElement::kExecutionState),
      .level_deps = {{on_to_wake_handling}},
      .strength = fuchsia_hardware_power::RequirementType::kPassive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration hardware_power_config = {
      {.element = hardware_power, .dependencies = {{passive_on_exec_state_wake_handling}}}};
  return hardware_power_config;
}
}  // namespace

zx_status_t Vim3::MaliInit() {
  {
    fpbus::Node aml_gpu_dev;
    aml_gpu_dev.name() = "aml_gpu";
    aml_gpu_dev.vid() = PDEV_VID_AMLOGIC;
    aml_gpu_dev.pid() = PDEV_PID_AMLOGIC_A311D;
    aml_gpu_dev.did() = PDEV_DID_AMLOGIC_MALI_INIT;
    aml_gpu_dev.mmio() = aml_gpu_mmios;

    fidl::Arena<> fidl_arena;
    fdf::Arena arena('MALI');

    auto aml_gpu_register_reset_node = fuchsia_driver_framework::ParentSpec{{
        .bind_rules =
            {
                fdf::MakeAcceptBindRule(bind_fuchsia_hardware_registers::SERVICE,
                                        bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
                fdf::MakeAcceptBindRule(bind_fuchsia_register::NAME,
                                        bind_fuchsia_amlogic_platform::NAME_REGISTER_MALI_RESET),
            },
        .properties =
            {
                fdf::MakeProperty(bind_fuchsia_hardware_registers::SERVICE,
                                  bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
                fdf::MakeProperty(bind_fuchsia_register::NAME,
                                  bind_fuchsia_amlogic_platform::NAME_REGISTER_MALI_RESET),
            },
    }};

    auto parents = std::vector<fuchsia_driver_framework::ParentSpec>{aml_gpu_register_reset_node};

    auto composite_node_spec = fuchsia_driver_framework::CompositeNodeSpec(
        {.name = "aml-gpu-composite", .parents = parents});

    auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
        fidl::ToWire(fidl_arena, aml_gpu_dev), fidl::ToWire(fidl_arena, composite_node_spec));
    if (!result.ok()) {
      zxlogf(ERROR, "AddCompositeNodeSpec Mali(aml-gpu-composite) request failed: %s",
             result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddCompositeNodeSpec Mali(aml-gpu-composite) failed: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  {
    fpbus::Node mali_dev;
    mali_dev.name() = "mali";
    mali_dev.vid() = PDEV_VID_ARM;
    mali_dev.pid() = PDEV_PID_GENERIC;
    mali_dev.did() = PDEV_DID_ARM_MAGMA_MALI;
    mali_dev.mmio() = mali_mmios;
    mali_dev.irq() = mali_irqs;
    mali_dev.bti() = mali_btis;
    mali_dev.power_config() = std::vector{hardware_power_config()};

    fidl::Arena<> fidl_arena;
    fdf::Arena arena('MALI');

    auto aml_gpu_bind_rules = std::vector{
        fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpu_mali::SERVICE,
                                bind_fuchsia_hardware_gpu_mali::SERVICE_DRIVERTRANSPORT)};

    auto aml_gpu_properties =
        std::vector{fdf::MakeProperty(bind_fuchsia_hardware_gpu_mali::SERVICE,
                                      bind_fuchsia_hardware_gpu_mali::SERVICE_DRIVERTRANSPORT)};

    auto parents =
        std::vector{fuchsia_driver_framework::ParentSpec(aml_gpu_bind_rules, aml_gpu_properties)};

    auto composite_node_spec =
        fuchsia_driver_framework::CompositeNodeSpec({.name = "mali-composite", .parents = parents});

    auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
        fidl::ToWire(fidl_arena, mali_dev), fidl::ToWire(fidl_arena, composite_node_spec));
    if (!result.ok()) {
      zxlogf(ERROR, "AddComposite Mali(mali_dev) request failed: %s",
             result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddComposite Mali(mali_dev) failed: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  return ZX_OK;
}

}  // namespace vim3
