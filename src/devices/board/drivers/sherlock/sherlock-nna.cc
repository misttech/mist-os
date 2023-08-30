// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/register/cpp/bind.h>
#include <soc/aml-common/aml-registers.h>
#include <soc/aml-t931/t931-hw.h>

#include "sherlock.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> sherlock_nna_mmios{
    {{
        .base = T931_NNA_BASE,
        .length = T931_NNA_LENGTH,
    }},
    {{
        .base = T931_HIU_BASE,
        .length = T931_HIU_LENGTH,
    }},
    {{
        .base = T931_POWER_DOMAIN_BASE,
        .length = T931_POWER_DOMAIN_LENGTH,
    }},
    {{
        .base = T931_MEMORY_PD_BASE,
        .length = T931_MEMORY_PD_LENGTH,
    }},
    {{
        .base = T931_NNA_SRAM_BASE,
        .length = T931_NNA_SRAM_LENGTH,
    }},
};

static const std::vector<fpbus::Bti> nna_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_NNA,
    }},
};

static const std::vector<fpbus::Irq> nna_irqs{
    {{
        .irq = T931_NNA_IRQ,
        .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    }},
};

static uint64_t s_external_sram_phys_base = T931_NNA_SRAM_BASE;

static const std::vector<fpbus::Metadata> nna_metadata{
    {{
        .type = 0,
        .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&s_external_sram_phys_base),
                                     reinterpret_cast<const uint8_t*>(&s_external_sram_phys_base) +
                                         sizeof(s_external_sram_phys_base)),
    }},
};

static const fpbus::Node nna_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "aml-nna";
  dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_T931;
  dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_NNA;
  dev.mmio() = sherlock_nna_mmios;
  dev.bti() = nna_btis;
  dev.irq() = nna_irqs;
  dev.metadata() = nna_metadata;
  return dev;
}();

zx_status_t Sherlock::NnaInit() {
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('NNA_');

  auto aml_nna_register_reset_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                                      bind_fuchsia_register::BIND_FIDL_PROTOCOL_DEVICE),
              fdf::MakeAcceptBindRule(
                  bind_fuchsia::REGISTER_ID,
                  bind_fuchsia_amlogic_platform::BIND_REGISTER_ID_NNA_RESET_LEVEL2),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                                bind_fuchsia_register::BIND_FIDL_PROTOCOL_DEVICE),
              fdf::MakeProperty(bind_fuchsia::REGISTER_ID,
                                bind_fuchsia_amlogic_platform::BIND_REGISTER_ID_NNA_RESET_LEVEL2),
          },
  }};

  auto aml_nna_composite_spec = fuchsia_driver_framework::CompositeNodeSpec{{
      .name = "aml_nna",
      .parents = {{aml_nna_register_reset_node}},
  }};

  fdf::WireUnownedResult result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, nna_dev), fidl::ToWire(fidl_arena, aml_nna_composite_spec));

  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Nna(nna_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Nna(nna_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

}  // namespace sherlock
