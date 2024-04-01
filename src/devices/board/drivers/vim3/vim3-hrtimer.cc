// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "src/devices/board/drivers/vim3/vim3.h"

namespace vim3 {

static const std::vector<fuchsia_hardware_platform_bus::Mmio> mmios{
    {{
        .base = A311D_TIMER_BASE,
        .length = A311D_TIMER_LENGTH,
    }},
};

static const std::vector<fuchsia_hardware_platform_bus::Irq> irqs{
    {{
        .irq = A311D_TIMER_A_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = A311D_TIMER_B_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = A311D_TIMER_C_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = A311D_TIMER_D_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = A311D_TIMER_F_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = A311D_TIMER_G_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = A311D_TIMER_H_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = A311D_TIMER_I_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

zx::result<> Vim3::HrTimerInit() {
  fuchsia_hardware_platform_bus::Node dev;
  dev.name() = "hrtimer";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_GENERIC;
  dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_HRTIMER;
  dev.mmio() = mmios;
  dev.irq() = irqs;

  // Power configuration.
  // fuchsia_hardware_power uses FIDL uint8 for power levels matching fuchsia_power_broker's.
  constexpr uint8_t kPowerLevelOff =
      static_cast<uint8_t>(fuchsia_power_broker::BinaryPowerLevel::kOff);
  constexpr uint8_t kPowerLevelOn =
      static_cast<uint8_t>(fuchsia_power_broker::BinaryPowerLevel::kOn);
  constexpr char kPowerElementName[] = "aml-hrtimer-wake";
  fuchsia_hardware_power::LevelTuple wake_handling_on = {{
      .child_level = kPowerLevelOn,
      .parent_level = static_cast<uint8_t>(fuchsia_power_system::WakeHandlingLevel::kActive),
  }};
  fuchsia_hardware_power::PowerDependency wake_handling = {{
      .child = kPowerElementName,
      .parent = fuchsia_hardware_power::ParentElement::WithSag(
          fuchsia_hardware_power::SagElement::kWakeHandling),
      .level_deps = {{std::move(wake_handling_on)}},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};
  fuchsia_hardware_power::PowerLevel off = {{.level = kPowerLevelOff, .name = "off"}};
  fuchsia_hardware_power::PowerLevel on = {{.level = kPowerLevelOn, .name = "on"}};
  fuchsia_hardware_power::PowerElement element = {
      {.name = kPowerElementName, .levels = {{std::move(off), std::move(on)}}}};
  fuchsia_hardware_power::PowerElementConfiguration config = {
      {.element = std::move(element), .dependencies = {{std::move(wake_handling)}}}};
  dev.power_config() = {std::move(config)};

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('HRTR');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, dev));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd (hrtimer) request failed: %s", result.FormatDescription().data());
    return result->take_error();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd (hrtimer) failed: %s", zx_status_get_string(result->error_value()));
    return result->take_error();
  }

  return zx::ok();
}

}  // namespace vim3
