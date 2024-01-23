// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.trippoint/cpp/wire.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-meson/g12b-clk.h>

#include "fidl/fuchsia.hardware.trippoint/cpp/wire_types.h"
#include "lib/driver/logging/cpp/logger.h"
#include "lib/fidl/cpp/wire/wire_types.h"
#include "vim3.h"
#include "zircon/status.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

const std::vector<fuchsia_driver_framework::BindRule> kClockInitRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_clock::BIND_INIT_STEP_CLOCK),
};
const std::vector<fuchsia_driver_framework::NodeProperty> kClockInitProps = std::vector{
    fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_clock::BIND_INIT_STEP_CLOCK),
};

const std::vector<fuchsia_driver_framework::ParentSpec> kParentSpecInit = std::vector{
    fuchsia_driver_framework::ParentSpec{{kClockInitRules, kClockInitProps}},
};

constexpr fuchsia_hardware_trippoint::wire::TripDeviceMetadata meta_pll = {
    .critical_temp_celsius = 101.0f,
};

const std::vector<fpbus::Irq> thermal_irqs_pll{
    {{
        .irq = A311D_TS_PLL_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

const std::vector<fpbus::Mmio> thermal_mmios_pll{
    {{
        .base = A311D_TEMP_SENSOR_PLL_BASE,
        .length = A311D_TEMP_SENSOR_PLL_LENGTH,
    }},
    {{
        .base = A311D_TEMP_SENSOR_PLL_TRIM,
        .length = A311D_TEMP_SENSOR_TRIM_LENGTH,
    }},
};

fpbus::Node thermal_node_pll = []() {
  fpbus::Node dev = {};
  dev.name() = "pll-temp-sensor";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_AMLOGIC_A311D;
  dev.did() = PDEV_DID_AMLOGIC_TEMP_SENSOR_PLL;
  dev.mmio() = thermal_mmios_pll;
  dev.irq() = thermal_irqs_pll;
  return dev;
}();

constexpr fuchsia_hardware_trippoint::wire::TripDeviceMetadata meta_ddr = {
    .critical_temp_celsius = 110.0f,
};

const std::vector<fpbus::Irq> thermal_irqs_ddr{
    {{
        .irq = A311D_TS_DDR_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

const std::vector<fpbus::Mmio> thermal_mmios_ddr{
    {{
        .base = A311D_TEMP_SENSOR_DDR_BASE,
        .length = A311D_TEMP_SENSOR_DDR_LENGTH,
    }},
    {{
        .base = A311D_TEMP_SENSOR_DDR_TRIM,
        .length = A311D_TEMP_SENSOR_TRIM_LENGTH,
    }},
};

fpbus::Node thermal_node_ddr = []() {
  fpbus::Node dev = {};
  dev.name() = "ddr-temp-sensor";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_AMLOGIC_A311D;
  dev.did() = PDEV_DID_AMLOGIC_TEMP_SENSOR_DDR;
  dev.mmio() = thermal_mmios_ddr;
  dev.irq() = thermal_irqs_ddr;
  return dev;
}();

zx_status_t Vim3::TemperatureInit() {
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('TEMP');

  clock_init_steps_.push_back(
      {g12b_clk::G12B_CLK_TSENSOR, fuchsia_hardware_clockimpl::wire::InitCall::WithEnable({})});

  {
    auto encoded_metadata = fidl::Persist(meta_ddr);
    if (encoded_metadata.is_error()) {
      zxlogf(ERROR, "Failed to encode Trip Point Metadata: %s",
             zx_status_get_string(encoded_metadata.error_value().status()));
      return encoded_metadata.error_value().status();
    }

    std::vector<fpbus::Metadata> trip_metadata_pll{
        {{
            .type = DEVICE_METADATA_TRIP,
            .data = encoded_metadata.value(),
        }},
    };

    thermal_node_ddr.metadata() = trip_metadata_pll;

    auto thermal_node_spec = fuchsia_driver_framework::CompositeNodeSpec{{
        "ddr-temp-sensor",
        kParentSpecInit,
    }};

    auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
        fidl::ToWire(fidl_arena, thermal_node_ddr), fidl::ToWire(fidl_arena, thermal_node_spec));

    if (!result.ok()) {
      zxlogf(ERROR, "%s: NodeAdd Temperature DDR failed: %s", __func__,
             result.FormatDescription().data());
      return result.status();
    }

    if (result->is_error()) {
      zxlogf(ERROR, "%s: NodeAdd Temperature DDR failed: %s", __func__,
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  {
    auto encoded_metadata = fidl::Persist(meta_pll);
    if (encoded_metadata.is_error()) {
      zxlogf(ERROR, "Failed to encode Trip Point Metadata: %s",
             zx_status_get_string(encoded_metadata.error_value().status()));
      return encoded_metadata.error_value().status();
    }

    std::vector<fpbus::Metadata> trip_metadata_pll{
        {{
            .type = DEVICE_METADATA_TRIP,
            .data = encoded_metadata.value(),
        }},
    };

    thermal_node_pll.metadata() = trip_metadata_pll;

    auto thermal_node_spec = fuchsia_driver_framework::CompositeNodeSpec{{
        "pll-temp-sensor",
        kParentSpecInit,
    }};

    auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
        fidl::ToWire(fidl_arena, thermal_node_pll), fidl::ToWire(fidl_arena, thermal_node_spec));

    if (!result.ok()) {
      zxlogf(ERROR, "%s: NodeAdd Temperature PLL failed: %s", __func__,
             result.FormatDescription().data());
      return result.status();
    }

    if (result->is_error()) {
      zxlogf(ERROR, "%s: NodeAdd Temperature PLL failed: %s", __func__,
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  return ZX_OK;
}

}  // namespace vim3
