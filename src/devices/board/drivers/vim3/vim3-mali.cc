// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/arm/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/registers/cpp/bind.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-common/aml-registers.h>

#include "src/devices/board/drivers/vim3/vim3.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

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
                fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                                        bind_fuchsia_hardware_registers::BIND_FIDL_PROTOCOL_DEVICE),
                fdf::MakeAcceptBindRule(bind_fuchsia::REGISTER_ID,
                                        bind_fuchsia_amlogic_platform::BIND_REGISTER_ID_MALI_RESET),
            },
        .properties =
            {
                fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                                  bind_fuchsia_hardware_registers::BIND_FIDL_PROTOCOL_DEVICE),
                fdf::MakeProperty(bind_fuchsia::REGISTER_ID,
                                  bind_fuchsia_amlogic_platform::BIND_REGISTER_ID_MALI_RESET),
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

    fidl::Arena<> fidl_arena;
    fdf::Arena arena('MALI');

    auto aml_gpu_bind_rules = std::vector{fdf::MakeAcceptBindRule(
        bind_fuchsia::PROTOCOL, bind_fuchsia_arm_platform::BIND_PROTOCOL_ARM_MALI)};

    auto aml_gpu_properties = std::vector{fdf::MakeProperty(
        bind_fuchsia::PROTOCOL, bind_fuchsia_arm_platform::BIND_PROTOCOL_ARM_MALI)};

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
